/**
 * A small Markdown-to-HTML renderer for Summaries and Notes: headings, bullet and task lists, paragraphs, bold,
 * italics and inline code. Everything is HTML-escaped first, so model output can never inject markup. Style the
 * output by putting it in a `.markdown` element (app.css).
 */
export function renderMarkdown(markdown: string): string {
  const out: string[] = [];
  let list: string[] = [];
  let paragraph: string[] = [];

  const flush = () => {
    if (paragraph.length) out.push(`<p>${paragraph.map(inline).join("<br>")}</p>`);
    if (list.length) out.push(`<ul>${list.join("")}</ul>`);
    paragraph = [];
    list = [];
  };

  for (const raw of markdown.split("\n")) {
    const line = raw.trimEnd();
    const heading = /^(#{1,6})\s+(.*)$/.exec(line);
    const item = /^\s*[-*+]\s+(?:\[( |x|X)\]\s+)?(.*)$/.exec(line);
    if (heading) {
      flush();
      const level = Math.min(heading[1].length + 1, 6);
      out.push(`<h${level}>${inline(heading[2])}</h${level}>`);
    } else if (item) {
      if (paragraph.length) flush();
      const box = item[1] === undefined ? "" : `<input type="checkbox" disabled${item[1] === " " ? "" : " checked"}> `;
      list.push(`<li>${box}${inline(item[2])}</li>`);
    } else if (line.trim() === "") {
      flush();
    } else {
      if (list.length) flush();
      paragraph.push(line.trim());
    }
  }
  flush();
  return out.join("\n");
}

function inline(text: string): string {
  return escape(text)
    .replace(/`([^`]+)`/g, "<code>$1</code>")
    .replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>")
    .replace(/(^|[^*])\*([^*\s][^*]*)\*/g, "$1<em>$2</em>");
}

export function escape(text: string): string {
  return text.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
}
