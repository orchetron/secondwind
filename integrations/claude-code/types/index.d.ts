export type Stats = {
  blocks: number
  seenTokens: number
  inputTokens: number
  outputTokens: number
  reopenedTokens: number
  verified: number
  failed: number
  isEstimated: boolean
}

export type Marker = {
  marker: string
  inputTokens: number
  outputTokens: number
  reopens: number
  at: number
}

declare module 'claude-code' {
  interface PluginState {
    secondwind: { enabled: boolean; stats: Stats; markers: Marker[] }
  }
}
