/** One worker as `mnemo2-mcp team-summary --json` reports it. */
export type SummaryWorker = {
  id: string
  name: string
  model: string
  model_short: string
  tier: string | null
  status: string
  needs_you: boolean
  elapsed_ms: number
}

/** The whole `team-summary` answer for the session's project. */
export type Summary = {
  workers: SummaryWorker[]
  open_plans: number
  running: number
  waiting: number
}

declare module 'claude-code' {
  interface PluginState {
    'mnemo-workers': { summary: Summary | null }
  }
}
