import { atom, read, update } from 'claude-code'
import type { EngineInterface, Register } from 'claude-code'

import type { Summary, SummaryWorker } from '../types'

const summary = atom({ plugin: 'mnemo-workers', key: 'summary' } as const, null)

/** Statuses in which a worker is doing work right now. */
const ACTIVE = new Set(['dispatching', 'working', 'fixing', 'merging'])
/** How many worker rows the band shows before "+N more". */
const MAX_ROWS = 6
const POLL_MS = 4000
/** A worker link's target; presses are answered here, never opened. */
const LINK = 'https://mnemo.invalid/worker/'
const MNEMO_TOOL = 'mcp__plugin_mnemo_mnemo__'

/** The summary JSON, or null for `{}`, bad output or no workers list. */
export function parseSummary(stdout: string): Summary | null {
  try {
    const value = JSON.parse(stdout.trim())
    return Array.isArray(value?.workers) ? (value as Summary) : null
  } catch {
    return null
  }
}

export function isActive(worker: SummaryWorker): boolean {
  return ACTIVE.has(worker.status)
}

/** The workers worth showing: active ones first, then the ones waiting on you. */
export function shownWorkers(value: Summary | null): SummaryWorker[] {
  if (!value) return []
  const active = value.workers.filter(isActive)
  const waiting = value.workers.filter(worker => !isActive(worker) && worker.needs_you)
  return [...active, ...waiting]
}

/** The status entry: "Mnemo ▶2 ◆1", or nothing when no worker needs a look. */
export function statusText(value: Summary | null): string | undefined {
  const workers = shownWorkers(value)
  if (workers.length === 0) return undefined
  const active = workers.filter(isActive).length
  return `Mnemo ▶${active} ◆${workers.length - active}`
}

export function elapsed(ms: number): string {
  const minutes = Math.floor(ms / 60000)
  if (minutes < 1) return `${Math.max(0, Math.floor(ms / 1000))}s`
  if (minutes < 60) return `${minutes}m`
  return `${Math.floor(minutes / 60)}h ${minutes % 60}m`
}

/** The band's markdown: one line per worker, the name a link that opens it. */
export function bandMarkdown(workers: SummaryWorker[]): string {
  const rows = workers.slice(0, MAX_ROWS).map(worker => {
    const glyph = isActive(worker) ? '▶' : '◆'
    const status = worker.status.replace(/_/g, ' ')
    const model = worker.model_short || worker.model || '?'
    return `- ${glyph} [${worker.name}](${LINK}${encodeURIComponent(worker.id)}) · ${model} · ${status} ${elapsed(worker.elapsed_ms)}`
  })
  const more = workers.length - MAX_ROWS
  if (more > 0) rows.push(`- +${more} more`)
  return rows.join('\n')
}

export function workerIdFromLink(href: string): string | null {
  return href.startsWith(LINK) ? decodeURIComponent(href.slice(LINK.length)) : null
}

/** The helper Claude Code's mnemo plugin installs under ~/.mnemo/bin. */
async function helperPath($: EngineInterface): Promise<string> {
  const profile = await $.env.get('USERPROFILE')
  if (profile) return `${profile}\\.mnemo\\bin\\mnemo2-mcp.exe`
  const home = await $.env.get('HOME')
  return home ? `${home}/.mnemo/bin/mnemo2-mcp` : 'mnemo2-mcp'
}

/** The last summary drawn, so an unchanged answer does not redraw. */
let last = ''

async function refresh($: EngineInterface): Promise<void> {
  let next: Summary | null = null
  try {
    const cwd = await $.session.cwd()
    const ran = await $.process.run(
      [await helperPath($), 'team-summary', '--json', '--cwd', cwd],
      { timeoutMs: 5000 },
    )
    next = ran.exitCode === 0 ? parseSummary(ran.stdout) : null
  } catch {
    next = null
  }
  const key = JSON.stringify(next)
  if (key === last) return
  last = key
  await update($, summary, () => next)
  $.ui.status(statusText(next))
}

async function openWorker($: EngineInterface, href: string): Promise<void> {
  const id = workerIdFromLink(href)
  if (!id) return
  try {
    const cwd = await $.session.cwd()
    const ran = await $.process.run(
      [await helperPath($), 'show-worker', id, '--cwd', cwd],
      { timeoutMs: 15000 },
    )
    if (ran.exitCode !== 0) {
      $.ui.toast(ran.stderr.trim().split('\n')[0] || `Could not open ${id} in Mnemo2`)
    }
  } catch {
    $.ui.toast(`Could not open ${id} in Mnemo2`)
  }
}

export const register: Register = on => {
  on('session.start', async ($, e, next) => {
    const started = await next(e)
    await refresh($)
    $.clock.every(POLL_MS, () => refresh($))
    return started
  })

  // A dispatch, merge or stop shows at once instead of on the next tick.
  on('tool.call', async ($, e, next) => {
    const result = await next(e)
    if (e.tool.startsWith(MNEMO_TOOL)) void refresh($)
    return result
  })

  on('ui.render', { component: 'AbovePrompt' }, async ($, e, next) => {
    const workers = shownWorkers(await read($, summary))
    if (e.props.hasSurvey || workers.length === 0) return next(e)

    const { Box, Markdown, Text } = $.ui.resolve(e)
    const text = bandMarkdown(workers)

    return (
      <Box flexDirection="column">
        <Text dimColor>Mnemo</Text>
        <Markdown
          key="mnemo-workers"
          text={text}
          onLinkPress={link => void openWorker($, link.href)}
        />
      </Box>
    )
  })
}
