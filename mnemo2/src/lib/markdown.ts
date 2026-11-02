import DOMPurify from "dompurify";
import hljs from "highlight.js/lib/common";
import MarkdownIt from "markdown-it";

const md = new MarkdownIt({
  html: false,
  linkify: true,
});

md.core.ruler.after("inline", "gfm_task_lists", (state) => {
  const lists: (typeof state.tokens)[number][] = [];
  for (let index = 0; index < state.tokens.length; index += 1) {
    const token = state.tokens[index];
    if (token.type === "bullet_list_open" || token.type === "ordered_list_open") {
      lists.push(token);
      continue;
    }
    if (token.type === "bullet_list_close" || token.type === "ordered_list_close") {
      lists.pop();
      continue;
    }
    if (
      token.type !== "list_item_open" ||
      state.tokens[index + 1]?.type !== "paragraph_open" ||
      state.tokens[index + 2]?.type !== "inline"
    ) {
      continue;
    }
    const inline = state.tokens[index + 2];
    const marker = /^\[([ xX])\](?:[\t ]+|$)/.exec(inline.content);
    const first = inline.children?.[0];
    if (!marker || first?.type !== "text" || !first.content.startsWith(marker[0])) {
      continue;
    }
    const checked = marker[1].toLowerCase() === "x";
    const checkbox = new state.Token("task_list_checkbox", "input", 0);
    checkbox.attrSet("class", "task-list-item-checkbox");
    checkbox.attrSet("type", "checkbox");
    checkbox.attrSet("disabled", "");
    checkbox.attrSet("aria-label", checked ? "Completed task" : "Incomplete task");
    if (checked) checkbox.attrSet("checked", "");
    first.content = first.content.slice(marker[0].length);
    if (first.content && inline.children) {
      inline.children.unshift(checkbox);
    } else if (inline.children) {
      inline.children[0] = checkbox;
    }
    token.attrJoin("class", "task-list-item");
    lists.at(-1)?.attrJoin("class", "contains-task-list");
  }
});

md.renderer.rules.task_list_checkbox = (tokens, index, options, _env, self) =>
  self.renderToken(tokens, index, options);

const escapeHtml = (value: string) => md.utils.escapeHtml(value);

function highlight(code: string, language: string): string {
  if (language && hljs.getLanguage(language)) {
    try {
      return hljs.highlight(code, { language }).value;
    } catch {
      // Fall through to escaped plain text for a broken grammar.
    }
  }
  return escapeHtml(code);
}

md.renderer.rules.fence = (tokens, index) => {
  const token = tokens[index];
  const language = token.info.trim().split(/\s+/)[0] || "text";
  const safeLanguage = escapeHtml(language);
  return `<div class="code-block"><div class="code-toolbar"><span>${safeLanguage}</span><button type="button" data-copy-code>Copy</button></div><pre><code class="hljs language-${safeLanguage}">${highlight(token.content, language)}</code></pre></div>`;
};

md.renderer.rules.link_open = (tokens, index, options, _env, self) => {
  const token = tokens[index];
  const href = String(token.attrGet("href") ?? "");
  if (/^https?:\/\//i.test(href)) {
    token.attrSet("target", "_blank");
    token.attrSet("rel", "noreferrer noopener");
  }
  return self.renderToken(tokens, index, options);
};

/** Render lead/worker Markdown to sanitized HTML safe for Svelte's html block. */
export function renderMarkdown(source: string): string {
  return DOMPurify.sanitize(md.render(source), {
    USE_PROFILES: { html: true },
    ADD_ATTR: ["data-copy-code", "target"],
  });
}
