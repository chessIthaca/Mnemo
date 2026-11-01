import { expect, test } from 'claude-code/testing'

import { bandSummary, RECENT_MS } from '../hooks/register'

const NOW = 1_800_000_000_000
const MINUTE = 60_000

const SUMMARY = {
  workers: [
    { id: 'gates33', name: 'gates33', model: 'Kimi/kimi-k3.1-pro', model_short: 'kimi', tier: 'difficult', status: 'working', needs_you: false, elapsed_ms: 720000, last_activity_ms: NOW - 30 * MINUTE },
    { id: 'skills33', name: 'skills33', model: 'Ollama Cloud/glm-5.3', model_short: 'glm', tier: 'average', status: 'in_review', needs_you: true, elapsed_ms: 60000, last_activity_ms: NOW - MINUTE },
    { id: 'audit26', name: 'Audit audit26', model: 'Ollama Cloud/glm-5.3', model_short: 'glm', tier: null, status: 'completed', needs_you: true, elapsed_ms: 90000, last_activity_ms: NOW - 10 * MINUTE },
    { id: 'asks40', name: 'asks40', model: 'Ollama Cloud/glm-5.3', model_short: 'glm', tier: 'average', status: 'needs_you', needs_you: true, elapsed_ms: 90000, last_activity_ms: NOW - 20 * MINUTE },
    { id: 'old01', name: 'old01', model: 'Ollama Cloud/glm-5.3', model_short: 'glm', tier: 'average', status: 'merged', needs_you: false, elapsed_ms: 1000, last_activity_ms: NOW - MINUTE },
  ],
  open_plans: 1,
  running: 1,
  waiting: 3,
}

const BAND = {
  plugin: 'mnemo-workers',
  component: 'AbovePrompt',
  props: { hasSurvey: false } as any,
} as const

type Ran = { argv: readonly string[] }

function engine(on: any, stdout: string, calls: Ran[]) {
  on('env.get', () => ({ value: 'C:\\Users\\me' }))
  on('session.cwd', () => ({ value: 'C:\\Mnemo' }))
  on('session.start', (_: unknown, e: { cwd: string }) => ({ cwd: e.cwd }))
  on('clock.now', () => ({ value: NOW }))
  on('clock.every', () => ({ value: {} }))
  on('ui.status', () => ({ value: undefined }))
  on('ui.toast', () => ({ value: undefined }))
  on('process.run', (_: unknown, e: Ran) => {
    calls.push(e)
    return { value: { exitCode: 0, stdout, stderr: '' } }
  })
}

test('the band lists active, blocked and just-finished workers and opens one', async ($, on) => {
  const calls: Ran[] = []
  engine(on, JSON.stringify(SUMMARY), calls)

  await $.session.start({ cwd: 'C:\\Mnemo', surface: 'desktop', isInteractive: true })
  const ui = await $.ui.mount({ ...BAND, surface: 'desktop' })

  const band = await ui.find({ key: 'mnemo-workers' })
  expect(band?.text).toContain('gates33')
  expect(band?.text).toContain('kimi')
  expect(band?.text).toContain('skills33')
  expect(band?.text).toContain('glm')
  expect(band?.text).toContain('asks40')
  expect(band?.text).not.toContain('audit26')
  expect(band?.text).not.toContain('old01')

  await ui.press({ key: 'mnemo-workers', link: { href: 'https://mnemo.invalid/worker/gates33' } })
  const shown = calls.find(call => call.argv[1] === 'show-worker')
  expect(shown?.argv.slice(1, 3)).toEqual(['show-worker', 'gates33'])
  await ui.unmount()
})

test('a finished worker leaves the band two minutes after its last activity', () => {
  const ids = (now: number) => bandSummary(SUMMARY, now)?.workers.map(worker => worker.id) ?? []

  expect(ids(NOW)).toEqual(['gates33', 'asks40', 'skills33'])
  expect(ids(NOW - MINUTE + RECENT_MS)).toContain('skills33')
  expect(ids(NOW - MINUTE + RECENT_MS + 1)).toEqual(['gates33', 'asks40'])
})

test('with nothing running, blocked or recent, there is no band', () => {
  const idle = { ...SUMMARY, workers: SUMMARY.workers.filter(worker => worker.status === 'completed' || worker.status === 'merged') }
  expect(bandSummary(idle, NOW)).toBeNull()
})

test('outside a Mnemo2 project the band stays out of the way', async ($, on) => {
  engine(on, '{}', [])
  on('ui.render', ($: any, e: any) => {
    const { Text } = $.ui.resolve(e)
    return <Text>engine</Text>
  })

  await $.session.start({ cwd: 'C:\\elsewhere', surface: 'desktop', isInteractive: true })
  const ui = await $.ui.mount({ ...BAND, surface: 'desktop' })

  expect(await ui.find({ key: 'mnemo-workers' })).toBeUndefined()
  await ui.unmount()
})
