import { unified } from "unified";
import remarkParse from "remark-parse";
import remarkGfm from "remark-gfm";
import { visit, SKIP } from "unist-util-visit";
import type { Root, Nodes } from "mdast";
import type { ResolvedFileLocation } from "../../runtime/types";
import { classifyReference, createHeadingSlugger, isInlineReference, type Reference } from "./references";

export interface MarkdownReference { key: string; value: Reference }
interface Options { base?: ResolvedFileLocation; prefix?: string; collect?: Map<string, MarkdownReference> }
function nodeText(node: Nodes): string {
  if ("value" in node) return node.value;
  if ("children" in node) return node.children.map((child) => nodeText(child as Nodes)).join("");
  return "";
}

export function remarkFileReferences(options: Options = {}) {
  return (tree: Root) => {
    // Preserve bare historical workspace URI support, without scanning code blocks.
    visit(tree, "text", (node, index, parent) => {
      if (index == null || !parent || parent.type === "link" || parent.type === "linkReference") return;
      const pattern = /workspace:\/\/[^\s<>"')\]]+/g;
      const segments: any[] = [];
      let last = 0;
      for (const match of node.value.matchAll(pattern)) {
        const url = match[0].replace(/[.,;!?:]+$/, "");
        if (match.index! > last) segments.push({ type: "text", value: node.value.slice(last, match.index) });
        segments.push({ type: "link", url, children: [{ type: "text", value: url }] });
        const trailing = match[0].slice(url.length);
        if (trailing) segments.push({ type: "text", value: trailing });
        last = match.index! + match[0].length;
      }
      if (!segments.length) return;
      if (last < node.value.length) segments.push({ type: "text", value: node.value.slice(last) });
      parent.children.splice(index, 1, ...segments);
      return [SKIP, index + segments.length];
    });
    visit(tree, "inlineCode", (node, index, parent) => {
      if (index == null || !parent || parent.type === "link" || parent.type === "linkReference" || !isInlineReference(node.value, Boolean(options.base))) return;
      parent.children.splice(index, 1, { type: "link", url: node.value, children: [node], data: { literalFilePath: true } } as any);
    });
    const definitions = new Map<string, string>();
    visit(tree, "definition", (node) => { definitions.set(node.identifier, node.url); });
    visit(tree, (node: any) => {
      let raw: string | undefined;
      if (node.type === "link" || node.type === "image") raw = node.url;
      if (node.type === "linkReference" || node.type === "imageReference") {
        raw = definitions.get(node.identifier);
        if (raw !== undefined) { node.type = node.type === "linkReference" ? "link" : "image"; node.url = raw; }
      }
      if (raw === undefined) return;
      const literal = node.data?.literalFilePath === true;
      const value = classifyReference(raw, options.base, literal);
      if (value.kind === "external") return;
      const key = JSON.stringify([raw, literal, options.base ?? null]);
      options.collect?.set(key, { key, value });
      node.data = { ...node.data, hProperties: { ...node.data?.hProperties, "data-file-reference": key } };
      // Keep local and rejected schemes away from native navigation and sanitization.
      node.url = "#";
    });
    const slug = createHeadingSlugger();
    visit(tree, "heading", (node) => {
      const name = slug(nodeText(node));
      node.data = { ...node.data, hProperties: { id: `${options.prefix ?? ""}${name}`, "data-heading-slug": name } };
    });
  };
}

export function collectMarkdownReferences(text: string, base?: ResolvedFileLocation): Map<string, MarkdownReference> {
  const collect = new Map<string, MarkdownReference>();
  const parser = unified().use(remarkParse).use(remarkGfm).use(remarkFileReferences, { base, collect });
  parser.runSync(parser.parse(text));
  return collect;
}
