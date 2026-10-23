import { expect, test } from 'claude-code/testing'

const PANE = {
  plugin: 'secondwind',
  component: 'Pane',
  requestId: 'secondwind',
  props: {
    title: 'secondwind',
    isFocused: false,
    bodyColumns: 120,
    placement: 'dock',
    scroll: { offset: 0, bodyRows: 20 },
    view: {},
  },
} as const

const BIG = 'row value=1 label=alpha\n'.repeat(300)
const STUB = `[${BIG.length} bytes offloaded, preview: row value=1 ...]\n<<swload:0123456789abcdef>>`

function answer(text: string, isError = false): string {
  return `${JSON.stringify({
    jsonrpc: '2.0',
    id: 1,
    result: { content: [{ type: 'text', text }], isError },
  })}\n`
}

type Sent = { name: string; arguments: Record<string, unknown> }

function engine(on: Parameters<Parameters<typeof test>[1]>[1], stdout = '', bash = BIG): Sent[] {
  const sent: Sent[] = []
  on('tool.register', () => ({ value: { tool: 'mcp__secondwind__recover' } }))
  on('command.register', () => ({ value: { command: 'secondwind' } }))
  on('ui.open', () => ({ value: { isPlaced: true } }))
  on('ui.status', () => ({ value: undefined }))
  on('session.start', (_, e) => ({ cwd: e.cwd }))
  on('process.run', (_, e) => {
    const argv = e.argv.join(' ')
    if (argv === 'printenv HOME') return { value: { exitCode: 0, stdout: '/nonexistent\n', stderr: '' } }
    if (argv === 'secondwind check') return { value: { exitCode: 0, stdout: '  ✓ state       /nonexistent/.secondwind (writable)\n', stderr: '' } }
    if (argv.startsWith('tail')) return { value: { exitCode: 1, stdout: '', stderr: '' } }
    const call = JSON.parse(e.init?.stdin ?? '{}')
    const name = call.params?.name
    sent.push(call.params)
    const text = name === 'optimize' ? stdout : name === 'resolve' ? BIG : 'unknown'
    return { value: { exitCode: 0, stdout: answer(text, name === 'unknown'), stderr: '' } }
  })
  on('tool.call', { tool: 'Bash' }, () => ({
    result: { stdout: bash, stderr: '', interrupted: false },
  }))
  return sent
}

test('large Bash output is replaced by the stub and the pane books it net', async ($, on) => {
  engine(on, STUB)
  await $.session.start({ cwd: '/', surface: 'desktop', isInteractive: true })
  expect((await $.command.run({ command: 'secondwind', args: 'on' })).text).toContain('on')

  const ran = await $.tool.call({ tool: 'Bash', command: 'cat big.txt' })
  expect(ran.deny).toBeUndefined()
  expect((ran.result as { stdout: string }).stdout).toBe(STUB)

  const back = await $.tool.call({ tool: 'mcp__secondwind__recover', marker: '<<swload:0123456789abcdef>>' })
  expect(back.deny).toBeUndefined()
  expect(JSON.stringify(back.result)).toContain('row value=1 label=alpha')

  for (const surface of ['terminal', 'desktop'] as const) {
    const ui = await $.ui.mount({ ...PANE, surface })
    expect((await ui.find({ type: 'Text', text: /^0\/1$/ }))?.text).toBe('0/1')
    expect((await ui.find({ type: 'Text', text: /seen$/ }))?.text).toContain('touched \u22481,800 of \u22483,600')
    expect((await ui.find({ type: 'Text', text: /×1/ }))?.text).toBe('×1')
    await ui.unmount()
  }
})

test('the binary hears which command produced the output, so it can keep a whole read in view', async ($, on) => {
  const sent = engine(on, STUB)
  await $.session.start({ cwd: '/', surface: 'desktop', isInteractive: true })
  await $.command.run({ command: 'secondwind', args: 'on' })

  await $.tool.call({ tool: 'Bash', command: 'cat big.txt' })
  const optimize = sent.find(call => call.name === 'optimize')
  expect(optimize?.arguments.tool).toBe('Bash')
  expect(optimize?.arguments.command).toBe('cat big.txt')
  expect(optimize?.arguments.block).toBe(BIG)
})

test('output under 512 bytes never reaches the binary', async ($, on) => {
  const small = 'x'.repeat(511)
  const sent = engine(on, STUB, small)
  await $.session.start({ cwd: '/', surface: 'desktop', isInteractive: true })
  await $.command.run({ command: 'secondwind', args: 'on' })

  const ran = await $.tool.call({ tool: 'Bash', command: 'ls' })
  expect((ran.result as { stdout: string }).stdout).toBe(small)
  expect(sent.filter(call => call.name === 'optimize')).toHaveLength(0)
})

test('small output and a verbatim answer pass through untouched', async ($, on) => {
  engine(on, BIG)
  await $.session.start({ cwd: '/', surface: 'desktop', isInteractive: true })
  await $.command.run({ command: 'secondwind', args: 'on' })

  const ran = await $.tool.call({ tool: 'Bash', command: 'cat big.txt' })
  expect((ran.result as { stdout: string }).stdout).toBe(BIG)
})

test('compression is off by default and the pane button turns it on for the session', async ($, on) => {
  engine(on, STUB)
  await $.session.start({ cwd: '/', surface: 'desktop', isInteractive: true })

  const off = await $.tool.call({ tool: 'Bash', command: 'cat big.txt' })
  expect((off.result as { stdout: string }).stdout).toBe(BIG)

  const ui = await $.ui.mount({ ...PANE, surface: 'terminal' })
  expect((await ui.find({ key: 'toggle' }))?.text).toContain('turn on')
  await ui.press({ key: 'toggle' })
  expect((await ui.find({ key: 'toggle' }))?.text).toContain('turn off')
  await ui.unmount()

  const on1 = await $.tool.call({ tool: 'Bash', command: 'cat big.txt' })
  expect((on1.result as { stdout: string }).stdout).toBe(STUB)

  expect((await $.command.run({ command: 'secondwind', args: 'off' })).text).toContain('off')
  const off2 = await $.tool.call({ tool: 'Bash', command: 'cat big.txt' })
  expect((off2.result as { stdout: string }).stdout).toBe(BIG)
})
