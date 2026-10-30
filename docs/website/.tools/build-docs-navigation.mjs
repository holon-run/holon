// Generate navigation from the same frontmatter and ordering as directory indexes.
import { readFile, readdir, writeFile } from "node:fs/promises";
import matter from "gray-matter";

const root = new URL("./dist/content/", import.meta.url);
const sections = [
  ["getting-started", "Getting started", "开始使用"],
  ["concepts", "Core concepts", "核心概念"],
  ["guides", "Task guides", "任务指南"],
  ["reference", "Reference", "配置与参考"],
];
const navigation = {};
for (const locale of ["en", "zh-CN"]) {
  const prefix = locale === "en" ? "" : "zh-CN/";
  navigation[locale] = [];
  for (const [section, en, zh] of sections) {
    const directory = `${prefix}${section}/`;
    const pages = [];
    for (const name of await readdir(new URL(directory, root))) {
      if (!name.endsWith(".md")) continue;
      const { data } = matter(await readFile(new URL(directory + name, root), "utf8"));
      if (data.draft) continue;
      if (typeof data.title !== "string" || !data.title.trim()) {
        throw new Error(`${directory}${name}: navigation needs a title`);
      }
      pages.push({
        title: data.title,
        href: `/${directory}${name === "README.md" ? "" : name.slice(0, -3)}`,
        order: typeof data.order === "number" ? data.order : Number.MAX_SAFE_INTEGER,
        overview: name === "README.md",
      });
    }
    pages.sort((a, b) => Number(b.overview) - Number(a.overview)
      || a.order - b.order || a.title.localeCompare(b.title, locale));
    navigation[locale].push({
      label: locale === "en" ? en : zh,
      pages: pages.map(({ title, href }) => ({ title, href })),
    });
  }
}
await writeFile(new URL("assets/docs-navigation.json", root), JSON.stringify(navigation));
console.log("Built documentation navigation for en and zh-CN.");
