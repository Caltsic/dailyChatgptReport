use super::*;
use std::net::Ipv4Addr;
use windows::Win32::{
    Foundation::{ERROR_BUFFER_OVERFLOW, ERROR_NO_DATA, NO_ERROR},
    NetworkManagement::{
        IpHelper::{
            GetAdaptersAddresses, GetIfEntry2, GAA_FLAG_INCLUDE_GATEWAYS,
            GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST,
            IP_ADAPTER_ADDRESSES_LH, MIB_IF_ROW2,
        },
        Ndis::IfOperStatusUp,
    },
    Networking::WinSock::{AF_INET, SOCKADDR_IN},
};

#[derive(serde::Serialize)]
pub struct NetworkAddress {
    name: String,
    address: String,
    #[serde(skip)]
    hardware: bool,
    #[serde(skip)]
    gateway: bool,
    #[serde(skip)]
    metric: u32,
}

pub fn addresses() -> Result<Vec<NetworkAddress>> {
    // Windows recommends a 15 KB initial buffer. u64 keeps the linked structs aligned.
    let mut buffer = vec![0u64; 15_000usize.div_ceil(8)];
    let flags = GAA_FLAG_INCLUDE_GATEWAYS | GAA_FLAG_SKIP_ANYCAST
        | GAA_FLAG_SKIP_DNS_SERVER | GAA_FLAG_SKIP_MULTICAST;
    loop {
        let mut size = (buffer.len() * 8) as u32;
        let result = unsafe {
            GetAdaptersAddresses(AF_INET.0 as u32, flags, None,
                Some(buffer.as_mut_ptr().cast()), &mut size)
        };
        if result == ERROR_BUFFER_OVERFLOW.0 {
            buffer.resize((size as usize).div_ceil(8), 0);
            continue;
        }
        if result == ERROR_NO_DATA.0 { return Ok(Vec::new()); }
        ensure!(result == NO_ERROR.0, "读取本机网卡失败：{}", std::io::Error::from_raw_os_error(result as i32));
        break;
    }
    let mut found = Vec::new();
    let mut pointer = buffer.as_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
    // Every address/name pointer belongs to buffer, which remains alive throughout traversal.
    unsafe {
        while let Some(adapter) = pointer.as_ref() {
            pointer = adapter.Next;
            if adapter.OperStatus != IfOperStatusUp { continue; }
            let mut row = MIB_IF_ROW2 { InterfaceLuid: adapter.Luid, ..Default::default() };
            let hardware = GetIfEntry2(&mut row) == NO_ERROR
                && row.InterfaceAndOperStatusFlags._bitfield & 1 != 0;
            let name = adapter.FriendlyName.to_string().unwrap_or_default();
            let mut unicast = adapter.FirstUnicastAddress;
            while let Some(item) = unicast.as_ref() {
                unicast = item.Next;
                let Some(socket) = item.Address.lpSockaddr.as_ref() else { continue; };
                if socket.sa_family != AF_INET { continue; }
                let socket = &*(item.Address.lpSockaddr.cast::<SOCKADDR_IN>());
                let ip = Ipv4Addr::from(socket.sin_addr.S_un.S_addr.to_ne_bytes());
                let octets = ip.octets();
                // Loopback, APIPA and proxy benchmark networks cannot serve LAN invitations.
                if ip.is_loopback() || ip.is_link_local() || ip.is_unspecified()
                    || (octets[0] == 198 && matches!(octets[1], 18 | 19)) { continue; }
                found.push(NetworkAddress { name: name.clone(), address: ip.to_string(),
                    hardware, gateway: !adapter.FirstGatewayAddress.is_null(), metric: adapter.Ipv4Metric });
            }
        }
    }
    // Prefer a connected physical LAN/Wi-Fi adapter over VPN and virtual adapters.
    found.sort_by_key(|a| (!a.hardware, !a.gateway, a.metric, a.address.clone()));
    Ok(found)
}

pub fn automatic(settings: &Value) -> bool {
    match settings["addressMode"].as_str() {
        Some(mode) => mode == "auto",
        None => s(settings, "advertisedUrl").is_empty() || url::Url::parse(s(settings, "advertisedUrl"))
            .ok().is_some_and(|url| matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]" | "::1"))),
    }
}

pub fn resolve(settings: &mut Value, addresses: &[NetworkAddress]) -> Result<()> {
    if automatic(settings) {
        let address = addresses.first().ok_or_else(|| anyhow!("未检测到可用内网 IP，请连接网络后重新检测，或手动填写组长地址"))?;
        settings["advertisedUrl"] = json!(format!("https://{}:{}", address.address, n(settings, "port")));
        settings["addressMode"] = json!("auto");
    }
    Ok(())
}

impl Leader {
    pub(super) fn network_info(&self) -> Result<Value> {
        let addresses = addresses()?;
        Ok(json!({"addresses":addresses}))
    }

    pub(super) fn refresh_address(&self) -> Result<Value> {
        let mut settings = self.settings()?;
        if automatic(&settings) {
            resolve(&mut settings, &addresses()?)?;
            self.store.write(|c| put(c, "DepartmentSettings", "settings", &settings, ""))?;
        }
        Ok(settings)
    }
}
