import { atom, read, update } from 'claude-code'
import type { Color, EngineInterface, Register } from 'claude-code'

import type { Marker, Stats } from '../types'

const PANE = 'secondwind'
const MIN_BYTES = 512
const MARKER = /<<swload:[0-9a-f]{16}>>/
const RECENT = 8
const ACCENT: Color = 'claude'
const ACCENT_HEX = '#d97757'
const WARN_HEX = '#d99a2b'
const INK = 'rgba(128,128,128,0.75)'
const TRACK_INK = 'rgba(128,128,128,0.4)'
const TRACK = 'rgba(128,128,128,0.18)'

const EMPTY: Stats = {
  blocks: 0,
  seenTokens: 0,
  inputTokens: 0,
  outputTokens: 0,
  reopenedTokens: 0,
  verified: 0,
  failed: 0,
  isEstimated: false,
}

const enabled = atom({ plugin: 'secondwind', key: 'enabled' } as const, false)
const stats = atom({ plugin: 'secondwind', key: 'stats' } as const, EMPTY)
const markers = atom({ plugin: 'secondwind', key: 'markers' } as const, [] as Marker[])

let eventsPath = ''
let resolveTool = 'mcp__secondwind__resolve'

type Answer = { text: string; isError: boolean }
type Event = { at_ms: number; surface: string; input_tokens: number; output_tokens: number; verified: boolean }

function rule(): string {
  return (
    'secondwind compresses large Bash output. A result that ends in a `<<swload:...>>` marker is a ' +
    `preview; the full original is behind the marker. Call \`${resolveTool}\` with that exact marker ` +
    'when you need the rest, with `select` (`L10-40`, `[3]/title`, `number=42/body`) to fetch one ' +
    'part instead of the whole block.'
  )
}

async function rpc($: EngineInterface, name: string, args: Record<string, unknown>): Promise<Answer> {
  const call = { jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name, arguments: args } }
  const ran = await $.process.run(['secondwind', 'mcp'], {
    stdin: `${JSON.stringify(call)}\n`,
    timeoutMs: 60_000,
  })
  if (ran.exitCode !== 0) throw new Error(ran.stderr.trim() || `secondwind mcp exited ${ran.exitCode}`)
  const line = ran.stdout.split('\n').find(l => l.includes('"id":1'))
  if (!line) throw new Error('secondwind mcp gave no answer')
  const result = JSON.parse(line).result ?? {}
  const text = ((result.content ?? []) as { text?: string }[]).map(c => c.text ?? '').join('')
  return { text, isError: result.isError === true }
}

async function ledgerPath($: EngineInterface): Promise<string> {
  const check = await $.process.run(['secondwind', 'check']).catch(() => undefined)
  const state = check && /state\s+(\S+)/.exec(check.stdout)?.[1]
  if (state) return `${state}/events/events.jsonl`
  const home = await $.process.run(['printenv', 'HOME']).catch(() => undefined)
  return home && home.exitCode === 0 && home.stdout.trim() ? `${home.stdout.trim()}/.secondwind/events/events.jsonl` : ''
}

async function lastEvent($: EngineInterface, since: number): Promise<Event | undefined> {
  if (!eventsPath) return undefined
  const ran = await $.process.run(['tail', '-n', '1', eventsPath])
  if (ran.exitCode !== 0 || !ran.stdout.trim()) return undefined
  const event = JSON.parse(ran.stdout.trim()) as Event
  return event.surface === 'mcp' && event.at_ms >= since ? event : undefined
}

async function bookReopen($: EngineInterface, marker: string, select: string, text: string): Promise<void> {
  const hit = (await read($, markers)).find(m => m.marker === marker)
  if (!hit) return
  const back = select ? Math.ceil(text.length / 4) : hit.inputTokens
  await update($, markers, all => all.map(m => (m.marker === marker ? { ...m, reopens: m.reopens + 1 } : m)))
  await update($, stats, s => ({
    ...EMPTY,
    ...s,
    reopenedTokens: s.reopenedTokens + back,
    isEstimated: s.isEstimated || Boolean(select),
  }))
  showStatus($, await current($), await read($, enabled))
}

function showStatus($: EngineInterface, s: Stats, isOn: boolean): void {
  $.ui.status(!isOn ? 'secondwind off' : s.blocks ? `secondwind ${pct(s)}% net` : 'secondwind on')
}

function net(s: Stats): number {
  return s.inputTokens - s.outputTokens - s.reopenedTokens
}

function pct(s: Stats): number {
  return share(net(s), s.inputTokens)
}

function share(part: number, whole: number): number {
  return whole === 0 ? 0 : Math.round((1000 * part) / whole) / 10
}

function estimate(text: string): number {
  return Math.ceil(text.length / 4)
}

// Session state outlives a reload, so a stats object saved by an older build may lack fields.
async function current($: EngineInterface): Promise<Stats> {
  const s = { ...EMPTY, ...(await read($, stats)) }
  return { ...s, seenTokens: Math.max(s.seenTokens, s.inputTokens + s.reopenedTokens) }
}

async function seen($: EngineInterface, tokens: number): Promise<void> {
  if (tokens > 0) await update($, stats, s => ({ ...EMPTY, ...s, seenTokens: (s.seenTokens ?? 0) + tokens }))
}

function ago(ms: number): string {
  const sec = Math.max(0, Math.round(ms / 1000))
  if (sec < 60) return `${sec}s`
  if (sec < 3600) return `${Math.round(sec / 60)}m`
  return `${Math.round(sec / 3600)}h`
}

function fmt(n: number): string {
  return n.toLocaleString('en-US')
}

function summary(s: Stats, isOn: boolean, list: Marker[]): string {
  const tilde = s.isEstimated ? '\u2248' : ''
  const head =
    `secondwind ${isOn ? 'on' : 'off'} in this session: ${s.blocks} blocks, ` +
    `${tilde}${fmt(s.inputTokens)} in, ${tilde}${fmt(s.outputTokens)} out, ` +
    `${tilde}${fmt(s.reopenedTokens)} reopened, net ${tilde}${fmt(net(s))} saved (${pct(s)}% of what it touched, ` +
    `\u2248${share(net(s), s.seenTokens)}% of \u2248${fmt(s.seenTokens)} tool-result tokens), ` +
    `${s.verified} verified${s.failed ? `, ${s.failed} failed` : ''}`
  const rows = list
    .slice(-8)
    .map(m => `  ${m.marker} ${fmt(m.inputTokens)}→${fmt(m.outputTokens)}${m.reopens ? ` reopened ×${m.reopens}` : ''}`)
  return rows.length ? `${head}\n${rows.join('\n')}` : head
}

export const register: Register = on => {
  on('session.start', async ($, e, next) => {
    const registered = await $.tool
      .register({
        name: 'recover',
        description:
          'Fetch the full content behind a secondwind `<<swload:...>>` marker, or one slice of it with `select`.',
        inputSchema: {
          type: 'object',
          properties: {
            marker: { type: 'string', description: 'The marker, exactly as it appears.' },
            select: {
              type: 'string',
              description:
                "Optional slice: `L<a>-<b>` for a line range, or a '/'-separated path of `field=value`, `[n]` and field names for JSON.",
            },
          },
          required: ['marker'],
        },
        isDeferred: false,
      })
      .then(
        () => true,
        () => false,
      )
    resolveTool = registered ? 'mcp__secondwind__recover' : 'mcp__secondwind__resolve'
    await $.command.register({
      name: 'secondwind',
      description: 'Turn secondwind compression on or off for this session, or show its savings',
      argumentHint: '[on|off|status|reset]',
    })
    eventsPath = await ledgerPath($)
    const isOn = await read($, enabled)
    showStatus($, await current($), isOn)
    if (isOn) void $.ui.open({ id: PANE, title: 'secondwind' })

    return next(e)
  })

  on('tool.call', async ($, e, next) => {
    const ran = await next(e)
    if (e.tool !== 'Bash' && ran.deny === undefined && (await read($, enabled))) await seen($, estimate(ran.text ?? ''))

    return ran
  })

  on('tool.call', { tool: 'Bash' }, async ($, e, next) => {
    const ran = await next(e)
    if (ran.deny !== undefined || ran.isError || !(await read($, enabled))) return ran
    const stdout = ran.result.stdout
    if (typeof stdout !== 'string') return ran
    if (stdout.length < MIN_BYTES) {
      await seen($, estimate(stdout))
      return ran
    }

    const since = Date.now()
    let answer: Answer
    try {
      answer = await rpc($, 'optimize', { block: stdout, tool: 'Bash', command: e.command })
    } catch {
      await update($, stats, s => ({ ...EMPTY, ...s, failed: s.failed + 1, seenTokens: s.seenTokens + estimate(stdout) }))
      return ran
    }
    if (answer.isError || answer.text === stdout) {
      await seen($, estimate(stdout))
      return ran
    }

    const event = await lastEvent($, since).catch(() => undefined)
    const inputTokens = event?.input_tokens ?? estimate(stdout)
    const outputTokens = event?.output_tokens ?? estimate(answer.text)
    await update($, stats, s => ({
      ...EMPTY,
      ...s,
      blocks: s.blocks + 1,
      seenTokens: s.seenTokens + inputTokens,
      inputTokens: s.inputTokens + inputTokens,
      outputTokens: s.outputTokens + outputTokens,
      verified: s.verified + (event?.verified ? 1 : 0),
      isEstimated: s.isEstimated || !event,
    }))
    const marker = MARKER.exec(answer.text)?.[0]
    if (marker) {
      await update($, markers, list =>
        [...list, { marker, inputTokens, outputTokens, reopens: 0, at: since }].slice(-50),
      )
    }
    showStatus($, await current($), true)

    return { result: { ...ran.result, stdout: answer.text }, context: ran.context }
  })

  on('tool.call', { tool: 'mcp__secondwind__recover' }, async ($, e) => {
    const { marker = '', select = '' } = e as { marker?: string; select?: string }
    const answer = await rpc($, 'resolve', { marker, select })
    if (!answer.isError) {
      await bookReopen($, marker, select, answer.text)
      await seen($, estimate(answer.text))
    }

    return { result: { content: [{ type: 'text', text: answer.text }], isError: answer.isError } }
  }).catch(() => ({
    result: { content: [{ type: 'text', text: 'secondwind: recover failed; is the binary installed?' }], isError: true },
  }))

  on('tool.call', { tool: 'mcp__secondwind__resolve' }, async ($, e, next) => {
    const ran = await next(e)
    if (ran.deny === undefined && !ran.isError && (await read($, enabled))) {
      const { marker = '', select = '' } = e as { marker?: string; select?: string }
      await bookReopen($, marker, select, ran.text ?? '')
    }

    return ran
  })

  on('command.run', { command: 'secondwind' }, async ($, e) => {
    const arg = e.args.trim()
    if (arg === 'on' || arg === 'off' || arg === 'resume' || arg === 'pause') {
      const isOn = arg === 'on' || arg === 'resume'
      await update($, enabled, () => isOn)
      showStatus($, await current($), isOn)
      if (isOn) void $.ui.open({ id: PANE, title: 'secondwind' })
      return { text: `secondwind compression ${isOn ? 'on' : 'off'} for this session.` }
    }
    if (arg === 'reset') {
      await update($, stats, () => EMPTY)
      await update($, markers, () => [])
      showStatus($, EMPTY, await read($, enabled))
      return { text: 'secondwind figures for this session reset.' }
    }
    await $.ui.open({ id: PANE, title: 'secondwind' })

    return { text: summary(await current($), await read($, enabled), await read($, markers)) }
  })

  on('prompt.compose', async ($, e, next) => {
    const { sections } = await next(e)
    if (!(await read($, enabled))) return { sections }

    return { sections: [...sections, { id: 'secondwind:markers', text: rule(), scope: 'session' }] }
  })

  on('ui.render', { component: 'Pane', requestId: PANE }, async ($, e) => {
    const table = $.ui.resolve(e)
    const { Box, Text, Button } = table
    const Svg = e.surface !== 'terminal' && 'Svg' in table ? table.Svg : undefined
    const [isOn, s, list] = await Promise.all([read($, enabled), current($), read($, markers)])
    const cols = e.props.bodyColumns
    const tilde = s.isEstimated ? '\u2248' : ''
    const percent = pct(s)
    const recent = [...list].reverse().slice(0, RECENT)
    const accent = isOn ? ACCENT : undefined
    const netTone: Color | undefined = !isOn ? undefined : net(s) < 0 ? 'error' : ACCENT
    const now = Date.now()

    const toggle = async () => {
      const next = !isOn
      await update($, enabled, () => next)
      showStatus($, s, next)
    }

    const peak = Math.max(1, s.inputTokens, s.outputTokens, s.reopenedTokens)
    const span = Math.max(12, Math.min(44, cols - 22))
    const bar = (key: string, value: number, color: string, theme: Color) => {
      const share = Math.min(1, value / peak)
      if (Svg) {
        const w = Math.max(60, Math.min(300, span * 7))
        const px = value > 0 ? Math.max(3, w * share) : 0
        return (
          <Svg
            key={key}
            alt={`${fmt(value)} tokens`}
            width={w}
            height={10}
            source={`<svg xmlns="http://www.w3.org/2000/svg" width="${w}" height="10" viewBox="0 0 ${w} 10"><rect width="${w}" height="10" rx="2" fill="${TRACK}"/>${px ? `<rect width="${px.toFixed(1)}" height="10" rx="2" fill="${color}"/>` : ''}</svg>`}
          />
        )
      }
      const full = value > 0 ? Math.max(1, Math.round(share * span)) : 0
      return (
        <Text>
          <Text color={theme}>{'█'.repeat(full)}</Text>
          <Text dimColor>{'░'.repeat(span - full)}</Text>
        </Text>
      )
    }

    const row = (key: string, label: string, value: number, color: string, theme: Color) => (
      <Box key={key} flexDirection="row" alignItems="center" gap={1}>
        <Box width={6}>
          <Text dimColor>{label}</Text>
        </Box>
        {bar(key, value, color, theme)}
        <Box flexGrow={1} justifyContent="flex-end">
          <Text>
            {tilde}
            {fmt(value)}
          </Text>
        </Box>
      </Box>
    )

    const tile = (key: string, label: string, value: string, color: Color | undefined) => (
      <Box key={key} flexDirection="column" borderStyle="round" borderDimColor paddingX={1} flexGrow={1} minWidth={8}>
        <Text dimColor>{label}</Text>
        <Text bold color={color} wrap="truncate-end">
          {value}
        </Text>
      </Box>
    )

    const num = (value: string, width: number) => (
      <Box width={width} justifyContent="flex-end">
        <Text>{value}</Text>
      </Box>
    )

    return (
      <Box flexDirection="column" paddingX={1}>
        <Box flexDirection="row" justifyContent="space-between" gap={1}>
          <Text>
            <Text color={accent}>{isOn ? '●' : '○'}</Text>
            <Text bold> {isOn ? 'on' : 'off'}</Text>
            <Text dimColor> in this session</Text>
          </Text>
          <Button key="toggle" plain hotkey="p" onPress={() => void toggle()}>
            {isOn ? '⏸ turn off' : '▶ turn on'}
          </Button>
        </Box>

        <Box flexDirection="row" gap={1} marginTop={1}>
          {tile('touched', 'touched', `${tilde}${percent}%`, netTone)}
          {tile('all', 'all output', `\u2248${share(net(s), s.seenTokens)}%`, netTone)}
          {tile('verified', 'verified', `${s.verified}/${s.blocks}`, s.blocks > 0 && s.verified < s.blocks ? 'warning' : undefined)}
        </Box>
        <Text dimColor wrap="truncate-end">
          net {tilde}
          {fmt(net(s))} tokens saved
        </Text>
        <Text dimColor wrap="truncate-end">
          touched {tilde}
          {fmt(s.inputTokens)} of ≈{fmt(s.seenTokens)} seen
        </Text>

        <Box flexDirection="column" marginTop={1}>
          {row('raw', 'raw', s.inputTokens, isOn ? INK : TRACK_INK, isOn ? 'text' : 'inactive')}
          {row('sent', 'sent', s.outputTokens, isOn ? ACCENT_HEX : TRACK_INK, accent ?? 'inactive')}
          {s.reopenedTokens > 0 && row('back', 'back', s.reopenedTokens, WARN_HEX, 'warning')}
        </Box>
        {(s.failed > 0 || s.isEstimated) && (
          <Text dimColor>
            {s.failed > 0 ? `${s.failed} failed` : ''}
            {s.failed > 0 && s.isEstimated ? ' · ' : ''}
            {s.isEstimated ? '\u2248 marks an estimate' : ''}
          </Text>
        )}

        <Box marginTop={1}>
          <Text dimColor>{list.length ? `recent` : isOn ? 'Bash output over 512 bytes lands here.' : 'Turn on to compress Bash output in this session.'}</Text>
        </Box>
        {recent.map(m => (
          <Box key={m.marker} flexDirection="row" gap={1}>
            <Box width={9}>
              <Text dimColor={m.reopens > 0}>{m.marker.slice(9, 17)}</Text>
            </Box>
            {num(fmt(m.inputTokens), 7)}
            <Text dimColor>→</Text>
            {num(fmt(m.outputTokens), 5)}
            <Box flexGrow={1} justifyContent="flex-end" minWidth={4}>
              <Text color={m.reopens > 0 ? 'warning' : undefined} dimColor={m.reopens === 0} wrap="truncate-end">
                {m.reopens > 0 ? `×${m.reopens}` : ago(now - m.at)}
              </Text>
            </Box>
          </Box>
        ))}
      </Box>
    )
  })
}
