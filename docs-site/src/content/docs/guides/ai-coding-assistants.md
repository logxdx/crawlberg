---
title: "AI Coding Assistants"
---

The Crawlberg plugin lets your coding agent crawl and scrape the web without leaving the chat — ask it to pull a page as Markdown, map a site, or extract metadata, and it drives Crawlberg for you.

**What the plugin does:** it bundles the Crawlberg agent skills (site crawling, HTML→Markdown scraping, headless-Chrome fallback) and wires up the `crawlberg` MCP server, so any major coding agent can call the crawler directly.

The plugin shells out to the `crawlberg` CLI. Install it from the [Installation](/getting-started/installation/) guide (for example, `brew install xberg-io/tap/crawlberg`) before driving the crawler from an assistant.

The plugin, its per-platform manifests, and version history live in this repository's self-hosted [`xberg-io/crawlberg`](https://github.com/xberg-io/crawlberg) marketplace.

:::note
MCP-capable integrations register the `crawlberg` MCP server for you. Pi uses skills and CLI access instead. To configure or run that server standalone, see the [MCP Server guide](/guides/mcp-server/) and the [MCP reference](/reference/mcp/).
:::

## Installing

Pick your harness below.

<details open>
<summary><strong>Claude Code</strong></summary>

```text
/plugin marketplace add xberg-io/crawlberg
/plugin install crawlberg@crawlberg
```

</details>

<details>
<summary><strong>Codex CLI</strong></summary>

```text
/plugins add https://github.com/xberg-io/crawlberg
```

Then search for `crawlberg` and select **Install Plugin**.
</details>

<details>
<summary><strong>Cursor</strong></summary>

Settings → Plugins → Add from URL → `https://github.com/xberg-io/crawlberg`, then select **crawlberg**.
</details>

<details>
<summary><strong>Gemini CLI</strong></summary>

```text
gemini extensions install https://github.com/xberg-io/crawlberg
```

</details>

<details>
<summary><strong>Factory Droid</strong></summary>

```text
droid plugin marketplace add https://github.com/xberg-io/crawlberg
droid plugin install crawlberg@crawlberg
```

</details>

<details>
<summary><strong>GitHub Copilot CLI</strong></summary>

```text
copilot plugin marketplace add https://github.com/xberg-io/crawlberg
copilot plugin install crawlberg@crawlberg
```

</details>

<details>
<summary><strong>Pi</strong></summary>

From a checkout of this repository:

```bash
pi install ./plugin
```

After the next plugin release, install the same package used by OpenCode:

```bash
pi install npm:@xberg-io/opencode-crawlberg
```

Pi loads the bundled skills and prompt templates. Use `/crawlberg-scrape` or `/crawlberg-crawl` with the input as arguments. Install the `crawlberg` CLI separately for shell access. This package does not register MCP servers, hooks, or native Pi extensions.

</details>

<details>
<summary><strong>opencode</strong></summary>

Add the package to `opencode.json`:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "plugin": ["@xberg-io/opencode-crawlberg"]
}
```

</details>
