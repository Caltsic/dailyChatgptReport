using System.Net;
using System.Text;
using DailyReport.Core;
using DocumentFormat.OpenXml;
using DocumentFormat.OpenXml.Packaging;
using DocumentFormat.OpenXml.Validation;
using W = DocumentFormat.OpenXml.Wordprocessing;
using Microsoft.Playwright;

namespace DailyReport.Server;

public sealed class Documents(string root)
{
    readonly SemaphoreSlim gate = new(1);
    public string BasePath(Report r) => SafePath(r.Id, Wire.Serialize(r));
    public string BasePath(DepartmentReport r) => SafePath(r.Id, Wire.Serialize(r));
    // Existing archived artifacts retain their path and layout across software updates.
    string SafePath(string id, string json) => Guid.TryParseExact(id, "N", out _) ? Path.Combine(root, "reports", id + "-" + Wire.Hash(json)[..16]) : throw new InvalidDataException("报告编号无效");
    public Task Export(Report r, CancellationToken ct = default) => Export(BasePath(r), r.MemberName + " · 当日工作汇报", ReportText.Sections(r).ToList(), ct);
    public Task Export(DepartmentReport r, CancellationToken ct = default) => Export(BasePath(r), r.Department + " · 部门当日工作汇总", ReportText.Sections(r).ToList(), ct);
    async Task Export(string path, string title, List<(string Heading, string Text)> sections, CancellationToken ct)
    {
        await gate.WaitAsync(ct);
        try
        {
            if (File.Exists(path + ".pdf") && File.Exists(path + ".docx")) return;
            Directory.CreateDirectory(Path.GetDirectoryName(path)!);
            string temporary = path + "." + Wire.Id();
            try
            {
                CreateWord(temporary + ".docx", title, sections);
                using var playwright = await Playwright.CreateAsync();
                var bundled = Path.Combine(AppContext.BaseDirectory, "browsers");
                string? executable = Directory.Exists(bundled) ? Path.Combine(bundled, "chromium", "chrome.exe") : null;
                await using var browser = await playwright.Chromium.LaunchAsync(new() { Headless = true, ExecutablePath = executable });
                await using var context = await browser.NewContextAsync(new() { JavaScriptEnabled = false });
                await context.RouteAsync("**/*", route => route.AbortAsync());
                var page = await context.NewPageAsync();
                await page.SetContentAsync(Html(title, sections)); ct.ThrowIfCancellationRequested();
                await page.PdfAsync(new() { Path = temporary + ".pdf", Format = "A4", PrintBackground = true, DisplayHeaderFooter = true, HeaderTemplate = "<span></span>", FooterTemplate = "<div style='font-size:9px;text-align:center;width:100%;color:#666'>第 <span class='pageNumber'></span> 页 / 共 <span class='totalPages'></span> 页</div>", Margin = new() { Top = "25.4mm", Bottom = "25.4mm", Left = "25.4mm", Right = "25.4mm" } });
                ct.ThrowIfCancellationRequested();
                File.Move(temporary + ".docx", path + ".docx", true); File.Move(temporary + ".pdf", path + ".pdf", true);
            }
            finally { foreach (var ext in new[] { ".docx", ".pdf" }) if (File.Exists(temporary + ext)) File.Delete(temporary + ext); }
        }
        finally { gate.Release(); }
    }
    public static string Html(string title, List<(string Heading, string Text)> sections)
    {
        var html = new StringBuilder("<!doctype html><html lang='zh-CN'><head><meta charset='utf-8'><style>body{font:11pt 'SimSun','宋体',serif;color:#333;line-height:1.35}h1{font:20pt 'Microsoft YaHei',sans-serif;color:#1f3864;margin:0 0 20pt}h2{font:14pt 'Microsoft YaHei',sans-serif;color:#1f3864;margin:16pt 0 8pt;break-after:avoid}p{white-space:pre-wrap;overflow-wrap:anywhere;margin:0 0 8pt;orphans:3;widows:3}table{width:100%;border-collapse:collapse;table-layout:fixed}td{border:1px solid #d9e0e8;padding:8pt;white-space:pre-wrap;overflow-wrap:anywhere}thead{display:table-header-group}</style></head><body><h1>");
        html.Append(WebUtility.HtmlEncode(title)).Append("</h1>");
        foreach (var section in sections)
        {
            if (section.Heading == "工作正文") { html.Append(MarkdownDocument.Html(section.Text)); continue; }
            html.Append("<h2>").Append(WebUtility.HtmlEncode(section.Heading)).Append("</h2>");
            if (IsSourceTable(section.Heading))
            {
                html.Append("<table><colgroup><col style='width:32%'><col style='width:68%'></colgroup><thead><tr><td>来源标识</td><td>记录位置／日报版本</td></tr></thead><tbody>");
                foreach (var (id, detail) in SourceRows(section.Text)) html.Append("<tr><td>").Append(WebUtility.HtmlEncode(id)).Append("</td><td>").Append(WebUtility.HtmlEncode(detail)).Append("</td></tr>");
                html.Append("</tbody></table>"); continue;
            }
            foreach (var paragraph in section.Text.Replace("\r\n", "\n").Split('\n')) html.Append("<p>").Append(WebUtility.HtmlEncode(paragraph.Length == 0 ? " " : paragraph)).Append("</p>");
        }
        return html.Append("</body></html>").ToString();
    }
    public static void CreateWord(string path, string title, List<(string Heading, string Text)> sections)
    {
        using var doc = WordprocessingDocument.Create(path, WordprocessingDocumentType.Document);
        var main = doc.AddMainDocumentPart(); var body = new W.Body(); main.Document = new W.Document(body);
        var styles = main.AddNewPart<StyleDefinitionsPart>();
        styles.Styles = new W.Styles(new W.DocDefaults(
            new W.RunPropertiesDefault(new W.RunPropertiesBaseStyle(new W.RunFonts { Ascii = "Calibri", HighAnsi = "Calibri", EastAsia = "SimSun" }, new W.Color { Val = "333333" }, new W.FontSize { Val = "22" }, new W.Languages { Val = "en-US", EastAsia = "zh-CN" })),
            new W.ParagraphPropertiesDefault(new W.ParagraphPropertiesBaseStyle(new W.SpacingBetweenLines { After = "160", Line = "324", LineRule = W.LineSpacingRuleValues.Auto }))));
        styles.Styles.Append(new W.Style(new W.StyleName { Val = "Normal" }) { Type = W.StyleValues.Paragraph, StyleId = "Normal", Default = true });
        styles.Styles.Append(new W.Style(new W.StyleName { Val = "Title" }, new W.BasedOn { Val = "Normal" }, new W.StyleParagraphProperties(new W.KeepNext(), new W.SpacingBetweenLines { After = "400" }), new W.StyleRunProperties(new W.RunFonts { EastAsia = "Microsoft YaHei" }, new W.Color { Val = "1F3864" }, new W.FontSize { Val = "40" })) { Type = W.StyleValues.Paragraph, StyleId = "Title" });
        styles.Styles.Append(new W.Style(new W.StyleName { Val = "heading 1" }, new W.BasedOn { Val = "Normal" }, new W.StyleParagraphProperties(new W.KeepNext(), new W.SpacingBetweenLines { Before = "320", After = "160" }, new W.OutlineLevel { Val = 0 }), new W.StyleRunProperties(new W.RunFonts { EastAsia = "Microsoft YaHei" }, new W.Bold(), new W.Color { Val = "1F3864" }, new W.FontSize { Val = "28" })) { Type = W.StyleValues.Paragraph, StyleId = "Heading1" });
        foreach (int level in new[] { 2, 3 }) styles.Styles.Append(new W.Style(new W.StyleName { Val = "heading " + level }, new W.BasedOn { Val = "Heading1" }, new W.StyleParagraphProperties(new W.KeepNext(), new W.OutlineLevel { Val = level - 1 }), new W.StyleRunProperties(new W.FontSize { Val = level == 2 ? "26" : "24" })) { Type = W.StyleValues.Paragraph, StyleId = "Heading" + level });
        body.Append(Paragraph(title, "Title"));
        foreach (var section in sections)
        {
            if (section.Heading == "工作正文") { foreach (var block in MarkdownDocument.Word(section.Text)) body.Append(block); continue; }
            body.Append(Paragraph(section.Heading, "Heading1"));
            if (IsSourceTable(section.Heading)) { body.Append(SourceTable(section.Text)); continue; }
            foreach (var line in section.Text.Replace("\r\n", "\n").Split('\n')) body.Append(Paragraph(line, "Normal"));
        }
        var footer = main.AddNewPart<FooterPart>();
        footer.Footer = new W.Footer(new W.Paragraph(new W.ParagraphProperties(new W.Justification { Val = W.JustificationValues.Center }), new W.Run(new W.Text("第 ")), new W.SimpleField { Instruction = "PAGE" }, new W.Run(new W.Text(" 页 / 共 ")), new W.SimpleField { Instruction = "NUMPAGES" }, new W.Run(new W.Text(" 页"))));
        body.Append(new W.SectionProperties(new W.FooterReference { Type = W.HeaderFooterValues.Default, Id = main.GetIdOfPart(footer) }, new W.PageSize { Width = 11906, Height = 16838 }, new W.PageMargin { Top = 1440, Bottom = 1440, Left = 1440, Right = 1440, Header = 720, Footer = 720, Gutter = 0 }));
        doc.PackageProperties.Title = title; doc.PackageProperties.Creator = "Codex 部门日报";
        main.Document.Save(); styles.Styles.Save(); footer.Footer.Save();
        var errors = new OpenXmlValidator().Validate(doc).Take(5).ToList();
        if (errors.Count > 0) throw new InvalidDataException("Word 结构校验失败：" + string.Join("；", errors.Select(e => e.Description)));
    }
    static bool IsSourceTable(string heading) => heading is "来源索引" or "来源日报版本";
    static IEnumerable<(string Id, string Detail)> SourceRows(string text)
    {
        foreach (string line in text.Replace("\r\n", "\n").Split('\n').Where(x => x.Length > 0))
        {
            int end = line.IndexOf("] ", StringComparison.Ordinal);
            yield return line.StartsWith('[') && end > 0 ? (line[1..end], line[(end + 2)..]) : ("", line);
        }
    }
    static W.Table SourceTable(string text)
    {
        var table = new W.Table(new W.TableProperties(
            new W.TableWidth { Width = "5000", Type = W.TableWidthUnitValues.Pct },
            new W.TableBorders(new W.TopBorder { Val = W.BorderValues.Single, Size = 4 }, new W.LeftBorder { Val = W.BorderValues.Single, Size = 4 }, new W.BottomBorder { Val = W.BorderValues.Single, Size = 4 }, new W.RightBorder { Val = W.BorderValues.Single, Size = 4 }, new W.InsideHorizontalBorder { Val = W.BorderValues.Single, Size = 4 }, new W.InsideVerticalBorder { Val = W.BorderValues.Single, Size = 4 }),
            new W.TableLayout { Type = W.TableLayoutValues.Fixed }),
            new W.TableGrid(new W.GridColumn { Width = "2888" }, new W.GridColumn { Width = "6138" }));
        W.TableCell Cell(string value, string width) => new(new W.TableCellProperties(new W.TableCellWidth { Width = width, Type = W.TableWidthUnitValues.Dxa }), Paragraph(value, "Normal"));
        table.Append(new W.TableRow(new W.TableRowProperties(new W.TableHeader()), Cell("来源标识", "2888"), Cell("记录位置／日报版本", "6138")));
        foreach (var (id, detail) in SourceRows(text)) table.Append(new W.TableRow(Cell(id, "2888"), Cell(detail, "6138")));
        return table;
    }
    static W.Paragraph Paragraph(string text, string style) => new(new W.ParagraphProperties(new W.ParagraphStyleId { Val = style }), new W.Run(new W.Text(CleanXml(text)) { Space = SpaceProcessingModeValues.Preserve }));
    static string CleanXml(string text) => new(text.Where(c => c is '\t' or '\n' or '\r' || c >= ' ').ToArray());
}
