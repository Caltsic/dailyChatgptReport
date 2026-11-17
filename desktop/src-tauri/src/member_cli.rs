#[tokio::main]
async fn main() {
    if let Err(error) = daily_report::cli::run(&std::env::args().skip(1).collect::<Vec<_>>()).await
    {
        eprintln!(
            "错误：{}",
            daily_report::common::redact(&format!("{error:#}"))
        );
        std::process::exit(1);
    }
}
