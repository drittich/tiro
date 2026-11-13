/** Find in a pane (Ctrl+F): matching text, and highlighting the matches in rendered content. */

export type FindOptions = { caseSensitive: boolean; wholeWord: boolean };

export type Match = { start: number; end: number };

/** Where `query` occurs in `text`, without overlaps. */
export function findInText(text: string, query: string, options: FindOptions): Match[] {
  if (!query) return [];
  const escaped = query.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const source = options.wholeWord ? `(?<![\\p{L}\\p{N}_])${escaped}(?![\\p{L}\\p{N}_])` : escaped;
  const pattern = new RegExp(source, options.caseSensitive ? "gu" : "giu");
  return [...text.matchAll(pattern)].map((m) => ({ start: m.index, end: m.index + m[0].length }));
}

/**
 * Where `query` occurs in the text under `root`, as Ranges; a match may cross inline elements (bold, links) but
 * not blocks. Text under `[data-nofind]`, such as a Transcript row's time, is skipped.
 */
export function findRanges(root: Element, query: string, options: FindOptions): Range[] {
  const nodes: { node: Text; at: number }[] = [];
  const blocks = new Map<Element, Element>();
  let text = "";
  let block: Element | null = null;
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
    acceptNode: (n) => (n.parentElement?.closest("[data-nofind]") ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_ACCEPT),
  });
  for (let node = walker.nextNode() as Text | null; node; node = walker.nextNode() as Text | null) {
    const b = blockOf(node.parentElement!, blocks);
    // A line break between blocks, so a match never runs from one paragraph into the next.
    if (b !== block) {
      text += "\n";
      block = b;
    }
    nodes.push({ node, at: text.length });
    text += node.data;
  }

  const ranges: Range[] = [];
  let k = 0;
  for (const { start, end } of findInText(text, query, options)) {
    while (k + 1 < nodes.length && nodes[k + 1].at <= start) k++;
    let j = k;
    while (j + 1 < nodes.length && nodes[j + 1].at < end) j++;
    const range = new Range();
    range.setStart(nodes[k].node, start - nodes[k].at);
    range.setEnd(nodes[j].node, end - nodes[j].at);
    ranges.push(range);
  }
  return ranges;
}

function blockOf(el: Element, cache: Map<Element, Element>): Element {
  const known = cache.get(el);
  if (known) return known;
  let b = el;
  while (b.parentElement && getComputedStyle(b).display.startsWith("inline")) b = b.parentElement;
  cache.set(el, b);
  return b;
}

/** Highlights the matches, `current` more strongly (styled by ::highlight(find) in app.css). */
export function paint(ranges: Range[], current: number) {
  if (!("highlights" in CSS)) return;
  CSS.highlights.set("find", new Highlight(...ranges));
  const at = ranges[current];
  if (!at) return void CSS.highlights.delete("find-current");
  const highlight = new Highlight(at);
  highlight.priority = 1;
  CSS.highlights.set("find-current", highlight);
}

export function unpaint() {
  if (!("highlights" in CSS)) return;
  CSS.highlights.delete("find");
  CSS.highlights.delete("find-current");
}

/** Scrolls each pane the Range is in, innermost first, so it is in view a third of the way down. */
export function scrollToRange(range: Range) {
  for (let el = range.startContainer.parentElement; el; el = el.parentElement) {
    if (el.scrollHeight <= el.clientHeight || !/auto|scroll/.test(getComputedStyle(el).overflowY)) continue;
    const box = el.getBoundingClientRect();
    const r = range.getBoundingClientRect();
    if (r.top < box.top || r.bottom > box.bottom) el.scrollTop += r.top - box.top - el.clientHeight / 3;
  }
}
