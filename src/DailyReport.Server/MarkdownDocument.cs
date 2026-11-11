using DocumentFormat.OpenXml;
using Markdig;
using Markdig.Syntax;
using Markdig.Syntax.Inlines;
using Table = Markdig.Extensions.Tables.Table;
using TableRow = Markdig.Extensions.Tables.TableRow;
using TableCell = Markdig.Extensions.Tables.TableCell;
using W = DocumentFormat.OpenXml.Wordprocessing;

namespace DailyReport.Server;

// Word and PDF use the same Markdown source. Member supplements remain plain verbatim text.
internal static class MarkdownDocument
{
    static readonly MarkdownPipeline Pipeline = new MarkdownPipelineBuilder().UsePipeTables().UseAutoLinks().UseCjkFriendlyEmphasis().DisableHtml().Build();
    public static string Html(string body) => Markdown.ToHtml(body, Pipeline);
    public static IEnumerable<OpenXmlElement> Word(string body) => Blocks(Markdown.Parse(body, Pipeline));
    static IEnumerable<OpenXmlElement> Blocks(ContainerBlock blocks, string prefix = "")
    {
        foreach (var block in blocks)
        {
            if (block is Table table) { yield return WordTable(table); continue; }
            if (block is ListBlock list)
            {
                int index = int.TryParse(list.OrderedStart, out int start) ? start : 1;
                foreach (ListItemBlock item in list) foreach (var child in Blocks(item, list.IsOrdered ? $"{index++}. " : "• ")) yield return child;
                continue;
            }
            if (block is ContainerBlock container) { foreach (var child in Blocks(container, prefix)) yield return child; continue; }
            if (block is LeafBlock leaf)
            {
                string style = block is HeadingBlock h ? "Heading" + Math.Min(h.Level, 3) : "Normal";
                var paragraph = new W.Paragraph(new W.ParagraphProperties(new W.ParagraphStyleId { Val = style }));
                if (prefix.Length > 0) { paragraph.Append(Text(prefix)); prefix = ""; }
                if (leaf.Inline != null) foreach (var run in Inlines(leaf.Inline)) paragraph.Append(run);
                else paragraph.Append(Text(leaf.Lines.ToString()));
                yield return paragraph;
            }
        }
    }
    static IEnumerable<OpenXmlElement> Inlines(ContainerInline container, bool bold = false, bool italic = false)
    {
        foreach (var inline in container)
        {
            switch (inline)
            {
                case LiteralInline text: yield return Text(text.Content.ToString(), bold, italic); break;
                case LineBreakInline: yield return new W.Run(new W.Break()); break;
                case CodeInline code: yield return Text(code.Content, bold, italic); break;
                case AutolinkInline link: yield return Text(link.Url, bold, italic); break;
                case LinkInline link:
                    foreach (var run in Inlines(link, bold, italic)) yield return run;
                    if (!string.IsNullOrEmpty(link.Url)) yield return Text("（" + link.Url + "）", bold, italic);
                    break;
                case EmphasisInline emphasis:
                    foreach (var run in Inlines(emphasis, bold || emphasis.DelimiterCount >= 2, italic || emphasis.DelimiterCount == 1)) yield return run;
                    break;
                case ContainerInline nested:
                    foreach (var run in Inlines(nested, bold, italic)) yield return run;
                    break;
            }
        }
    }
    static W.Run Text(string value, bool bold = false, bool italic = false)
    {
        var run = new W.Run(); var properties = new W.RunProperties();
        if (bold) properties.Append(new W.Bold()); if (italic) properties.Append(new W.Italic());
        run.Append(properties, new W.Text(new string(value.Where(c => c is '\t' or '\n' or '\r' || c >= ' ').ToArray())) { Space = SpaceProcessingModeValues.Preserve }); return run;
    }
    static W.Table WordTable(Table source)
    {
        int count = Math.Max(1, source.OfType<TableRow>().Select(r => r.Count).DefaultIfEmpty(1).Max());
        var result = new W.Table(new W.TableProperties(new W.TableWidth { Width = "5000", Type = W.TableWidthUnitValues.Pct },
            new W.TableBorders(new W.TopBorder { Val = W.BorderValues.Single, Size = 4 }, new W.LeftBorder { Val = W.BorderValues.Single, Size = 4 }, new W.BottomBorder { Val = W.BorderValues.Single, Size = 4 }, new W.RightBorder { Val = W.BorderValues.Single, Size = 4 }, new W.InsideHorizontalBorder { Val = W.BorderValues.Single, Size = 4 }, new W.InsideVerticalBorder { Val = W.BorderValues.Single, Size = 4 }), new W.TableLayout { Type = W.TableLayoutValues.Fixed }),
            new W.TableGrid(Enumerable.Range(0, count).Select(_ => new W.GridColumn { Width = (9026 / count).ToString() })));
        foreach (TableRow row in source)
        {
            var output = new W.TableRow(); if (row.IsHeader) output.Append(new W.TableRowProperties(new W.TableHeader()));
            foreach (TableCell cell in row)
            {
                var outputCell = new W.TableCell(new W.TableCellProperties(new W.TableCellWidth { Width = (9026 / count).ToString(), Type = W.TableWidthUnitValues.Dxa }));
                foreach (var child in Blocks(cell)) outputCell.Append(child);
                if (outputCell.LastChild is not W.Paragraph) outputCell.Append(new W.Paragraph());
                output.Append(outputCell);
            }
            result.Append(output);
        }
        return result;
    }
}
