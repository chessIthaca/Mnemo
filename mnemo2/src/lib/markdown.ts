import DOMPurify from "dompurify";
import hljs from "highlight.js/lib/common";
import MarkdownIt from "markdown-it";

const md = new MarkdownIt({
  html: false,
  linkify: true,
  typographer: true,
});

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
