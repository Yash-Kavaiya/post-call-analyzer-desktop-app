/**
 * A stand-in for the Rust command layer, used only by `npm run dev` in a plain
 * browser.
 *
 * The reason this exists: every screen in this app is driven by Tauri IPC, so
 * without a backend the UI renders nothing but error states — which makes it
 * impossible to look at a layout, check a colour in both themes, or see what
 * the scorecard panel does with real-shaped data before the agent has ever run.
 * Installing a fake `__TAURI_INTERNALS__` is the smallest hook that fixes that:
 * `api.ts` and every component stay untouched and unaware.
 *
 * Two guards keep this out of the product: it is behind `import.meta.env.DEV`
 * (so Vite drops it from any production bundle) and it refuses to install if a
 * real Tauri runtime is already present.
 */

import type {
  AppSettings,
  CallDetail,
  CallSummary,
  Finding,
  FindingCount,
  MonthPoint,
  PersonDetail,
  PersonSummary,
  ProviderInfo,
  RunItem,
  RunSummary,
  Scorecard,
  Stats,
  Startup,
} from '../api'
import type { PeakLevel, Segment, WaveformPeaks, Word } from '../components/waveform'

// ---------------------------------------------------------------- generators

/**
 * Deterministic noise. Fixtures must look identical on every reload, or a
 * visual comparison between two runs is meaningless.
 */
function rng(seed: number): () => number {
  let s = seed >>> 0
  return () => {
    s = (s * 1664525 + 1013904223) >>> 0
    return s / 0x100000000
  }
}

/**
 * Peaks shaped like a two-party conversation: alternating stretches of speech
 * separated by short gaps, with one long hold in the middle.
 */
function makePeaks(durationSecs: number): WaveformPeaks {
  const random = rng(20260811)

  const level = (bucketSecs: number): PeakLevel => {
    const buckets = Math.ceil(durationSecs / bucketSecs)
    const minMax: number[] = []
    const energy: number[] = []

    for (let i = 0; i < buckets; i += 1) {
      const t = i * bucketSecs
      // Turns of roughly 9s, plus a 14s hold two thirds of the way in.
      const inHold = t > durationSecs * 0.62 && t < durationSecs * 0.62 + 14
      const speaking = !inHold && Math.sin(t / 9) > -0.35
      const envelope = speaking ? 0.35 + random() * 0.5 : random() * 0.04
      const amp = envelope * (0.75 + 0.25 * Math.sin(t / 2.3))
      minMax.push(-amp, amp)
      energy.push(speaking ? Math.min(1, envelope * 1.4) : envelope)
    }
    return { bucketSecs, minMax, energy }
  }

  return {
    durationSecs,
    levels: [level(1), level(0.25), level(0.05)],
  }
}

/** Split a line into words spread evenly across the segment's span. */
function words(text: string, start: number, end: number, seed: number): Word[] {
  const random = rng(seed)
  const tokens = text.split(' ')
  const step = (end - start) / Math.max(1, tokens.length)
  return tokens.map((text, i) => ({
    text,
    start: start + i * step,
    end: start + (i + 1) * step,
    // A few low-confidence words so the "uncertain" underline is visible.
    confidence: random() < 0.12 ? 0.42 + random() * 0.15 : 0.86 + random() * 0.13,
  }))
}

const SCRIPT: [number, string, number][] = [
  [0, 'Thank you for calling Northwind Support, my name is Priya, how can I help you today?', 0],
  [6.4, 'Hi Priya, my card was charged twice for the same order and I need that sorted out.', 1],
  [13.1, "I'm sorry that happened, that's frustrating. Let me pull up the order right away.", 0],
  [19.8, 'It was order 4417, placed on the second.', 1],
  [24.2, 'Thank you. I can see two authorisations against [CARD_1] for the same amount.', 0],
  [32.6, 'So you can see it too. How long will the refund take?', 1],
  [37.4, 'I can release the duplicate now. It clears in three to five working days.', 0],
  [44.9, 'And can you confirm the number you have on file, is it still [PHONE_1]?', 0],
  [51.2, "Yes that's still current.", 1],
  [54.0, 'Perfect. I have raised the reversal, reference RV-88214.', 0],
  [61.5, 'Let me put you on a brief hold while I confirm it went through.', 0],
  [78.4, 'Thanks for holding. The reversal is confirmed and you will get an email shortly.', 0],
  [86.2, 'That was quicker than I expected. Thank you.', 1],
  [90.1, 'Is there anything else I can help with today?', 0],
  [93.8, 'No, that covers it.', 1],
  [96.0, 'Thank you for calling Northwind, have a good afternoon.', 0],
]

const DURATION = 104

function makeSegments(): Segment[] {
  return SCRIPT.map(([start, text, speaker], i) => {
    const end = i + 1 < SCRIPT.length ? Math.min(SCRIPT[i + 1][0] - 0.3, start + 12) : DURATION - 1
    const redacted = /\[CARD_1\]|\[PHONE_1\]/.test(text)
    return {
      id: `seg_${i + 1}`,
      speakerIndex: speaker,
      role: speaker === 0 ? 'agent' : 'customer',
      start,
      end,
      // The stored `text` is the raw transcript; `redactedText` is what leaves
      // the machine. The fixture shows the redacted form, as the app does.
      text,
      redactedText: redacted ? text : null,
      words: words(text, start, end, 1000 + i),
      confidence: 0.88 + (i % 5) * 0.02,
      language: 'en',
    } satisfies Segment
  })
}

const SEGMENTS = makeSegments()

const CALLS: CallSummary[] = [
  {
    id: 'call_demo_1',
    displayName: 'northwind-support-4417.m4a',
    stage: 'analyzed',
    durationSecs: DURATION,
    createdAt: '2026-08-10T09:14:00Z',
    overall: 82.5,
    sentiment: 0.42,
    compliancePassed: true,
    blockingFindings: 0,
    agentName: 'Priya Sharma',
    contactName: 'Ishita AI Cloud Koradia',
    occurredAt: '2025-11-12T14:23:26Z',
    speakerCount: 2,
  },
  ...Array.from({ length: 11 }, (_, i) => {
    const stages = ['analyzed', 'redacted', 'diarized', 'transcribed', 'decoded', 'failed']
    const stage = stages[i % stages.length]
    const analysed = stage === 'analyzed'
    // Every column state a row can be in appears somewhere in this fixture:
    // scored, unassessed, neutral, negative, passing and failing. A list where
    // they all look the same cannot be checked by looking at it.
    const failing = analysed && i % 3 === 0
    return {
      id: `call_demo_${i + 2}`,
      displayName: `inbound-2026-08-${String(9 - (i % 8)).padStart(2, '0')}-${1200 + i}.m4a`,
      stage,
      durationSecs: 96 + i * 37,
      createdAt: `2026-08-0${(i % 8) + 1}T11:0${i % 6}:00Z`,
      overall: analysed ? 61 + ((i * 7) % 34) : null,
      sentiment: analysed ? Math.round((0.55 - (i % 5) * 0.32) * 100) / 100 : null,
      compliancePassed: analysed ? !failing : null,
      blockingFindings: failing ? 1 + (i % 2) : 0,
      // Half attributed, half not: an unattributed call is a normal row and
      // has to look like one rather than like a broken one.
      agentName: i % 2 === 0 ? 'Priya Sharma' : null,
      contactName: i % 3 === 0 ? null : `+9198${40000000 + i}`,
      occurredAt: i % 4 === 0 ? null : `2025-${String((i % 11) + 1).padStart(2, '0')}-14T10:30:00Z`,
      speakerCount: stage === 'failed' ? null : 2,
    } satisfies CallSummary
  }),
]

/**
 * The durable ledger for the demo call.
 *
 * Deliberately holds one blocking and one advisory finding: the risk banner must
 * be seen to show the first and ignore the second, which is the whole point of
 * it being separate from the compliance panel.
 */
const FINDINGS: Finding[] = [
  {
    id: 'find_demo_1',
    callId: 'call_demo_1',
    agent: 'compliance',
    label: 'Compliance & risk',
    code: 'HOLD_NOT_TIMEBOXED',
    title: 'Hold placed without an expected duration',
    detail:
      'The agent announced a hold at 1:01 without saying how long it would take. Policy requires an estimate before silence.',
    severity: 'high',
    blocking: true,
    citations: [{ segmentId: 'seg_11', start: 61.5, end: 66.2, quote: 'brief hold' }],
  },
  {
    id: 'find_demo_2',
    callId: 'call_demo_1',
    agent: 'compliance',
    label: 'Compliance & risk',
    code: 'NO_SATISFACTION_CHECK',
    title: 'Closed without checking for anything further',
    detail: 'The call ended on the reference number with no "anything else I can help with".',
    severity: 'low',
    blocking: false,
    citations: [{ segmentId: 'seg_13', start: 86.2, end: 90, quote: 'quicker than I expected' }],
  },
]

const RISK_REGISTER: FindingCount[] = [
  {
    code: 'PCI_PAN',
    title: 'Full card number spoken aloud',
    severity: 'critical',
    calls: 4,
    blocking: true,
  },
  {
    code: 'HOLD_NOT_TIMEBOXED',
    title: 'Hold placed without an expected duration',
    severity: 'high',
    calls: 31,
    blocking: true,
  },
  {
    code: 'MISSING_DISCLOSURE',
    title: 'Company and agent not named at open',
    severity: 'medium',
    calls: 12,
    blocking: false,
  },
  {
    code: 'NO_SATISFACTION_CHECK',
    title: 'Closed without checking for anything further',
    severity: 'low',
    calls: 47,
    blocking: false,
  },
]

const SCORECARD: Scorecard = {
  callId: 'call_demo_1',
  overall: 82.5,
  rubricId: 'default',
  rubricVersion: '1',
  summary:
    'Priya opened correctly, acknowledged the duplicate charge without being prompted, and closed the loop with a reference number. The hold was announced but not time-boxed.',
  criteria: [
    {
      key: 'greeting',
      label: 'Greeting & Identification',
      score: 1,
      weight: 1,
      reasoning: 'Named the company and herself in the opening line and invited the issue.',
      notApplicable: false,
      citations: [
        {
          segmentId: 'seg_1',
          start: 0,
          end: 6.1,
          quote: 'Thank you for calling Northwind Support, my name is Priya',
        },
      ],
    },
    {
      key: 'active_listening',
      label: 'Active Listening',
      score: 0.85,
      weight: 1.5,
      reasoning:
        'Repeated the order number back and confirmed the card on file, but talked over the customer once at the hold.',
      notApplicable: false,
      citations: [
        {
          segmentId: 'seg_5',
          start: 24.2,
          end: 32.3,
          quote: 'I can see two authorisations against [CARD_1]',
        },
      ],
    },
    {
      key: 'empathy',
      label: 'Empathy',
      score: 0.75,
      weight: 1.5,
      reasoning: 'Acknowledged the frustration once, early. No further acknowledgement after the hold.',
      notApplicable: false,
      citations: [
        {
          segmentId: 'seg_3',
          start: 13.1,
          end: 19.5,
          quote: "I'm sorry that happened, that's frustrating",
        },
      ],
    },
    {
      key: 'resolution',
      label: 'Resolution',
      score: 0.9,
      weight: 2,
      reasoning: 'Raised the reversal on the call and gave a reference plus a realistic timeframe.',
      notApplicable: false,
      citations: [
        { segmentId: 'seg_10', start: 54, end: 61.2, quote: 'reference RV-88214' },
        {
          segmentId: 'seg_12',
          start: 78.4,
          end: 85.9,
          quote: 'The reversal is confirmed',
        },
      ],
    },
    {
      key: 'professionalism',
      label: 'Professionalism',
      score: 0,
      weight: 1,
      reasoning:
        'No policy or compliance statement appears in the transcript. (no verifiable citation; excluded from score)',
      notApplicable: true,
      citations: [],
    },
  ],
}

const METRICS = {
  speakers: [
    { speakerIndex: 0, role: 'agent', speechSecs: 61.4, talkShare: 0.688, turns: 10,
      words: 151, wordsPerMinute: 147, questions: 2, longestTurnSecs: 12.0 },
    { speakerIndex: 1, role: 'customer', speechSecs: 27.9, talkShare: 0.312, turns: 6,
      words: 59, wordsPerMinute: 127, questions: 1, longestTurnSecs: 8.2 },
  ],
  interruptions: [
    { at: 61.5, bySpeaker: 0, overSpeaker: 1, overlapSecs: 1.4 },
  ],
  silences: [
    { start: 66.2, end: 78.4, secs: 12.2, afterSpeaker: 0 },
  ],
  monologues: [],
  keywords: [
    { term: 'payment', count: 4, firstAt: 6.4 },
    { term: 'refund', count: 3, firstAt: 32.6 },
    { term: 'order', count: 3, firstAt: 19.8 },
    { term: 'reversal', count: 2, firstAt: 54.0 },
    { term: 'charged', count: 2, firstAt: 6.4 },
  ],
  totalSilenceSecs: 12.2,
  silenceRatio: 0.117,
  longestSilenceSecs: 12.2,
  agentTalkShare: 0.688,
}

const DETAIL: CallDetail = {
  id: 'call_demo_1',
  displayName: 'northwind-support-4417.m4a',
  durationSecs: DURATION,
  engine: 'whisper.cpp',
  model: 'ggml-small.en',
  mediaPath: null,
  metrics: METRICS as CallDetail['metrics'],
  segments: SEGMENTS,
  speakers: [
    { index: 0, role: 'agent', roleConfidence: 0.91, totalSpeechSecs: 61.4, turnCount: 10 },
    { index: 1, role: 'customer', roleConfidence: 0.88, totalSpeechSecs: 27.9, turnCount: 6 },
  ],
  features: {
    meanPitchHz: 178.4,
    pitchStddevHz: 31.2,
    jitterPct: 1.7,
    meanRmsDb: -21.6,
    speechRateWpm: 147,
    silenceRatio: 0.14,
    crosstalkRatio: 0.03,
    longestPauseSecs: 14.2,
  },
}

const STATS: Stats = {
  totalCalls: 1968,
  outstanding: 1956,
  byStage: {
    discovered: 1956,
    decoded: 1,
    transcribed: 1,
    diarized: 1,
    redacted: 4,
    analyzed: 4,
    failed: 1,
  },
  inputTokens: 41_284,
  outputTokens: 6_912,
  costUsd: 0.1743,
  audioSecs: 478_000,
}

const STARTUP: Startup = {
  whisperBinary: 'vendor/whisper/Release/whisper-cli.exe',
  whisperBinaryPresent: true,
  whisperModel: 'models/ggml-small.en.bin',
  whisperModelPresent: true,
  englishOnly: true,
}

// -------------------------------------------------------------- run history

/**
 * Past runs. Fixed timestamps rather than offsets from `now`, so the history
 * looks the same on every reload and two screenshots can be compared.
 */
const RUNS: RunSummary[] = [
  {
    id: 'job_demo_3',
    kind: 'transcribe',
    label: 'Transcribe & redact',
    targetStage: 'redacted',
    limitN: 10,
    concurrency: 2,
    total: 10,
    processed: 9,
    failed: 1,
    status: 'done',
    startedAt: '2026-08-10T21:04:11Z',
    finishedAt: '2026-08-10T21:31:48Z',
    itemCount: 10,
  },
  {
    id: 'job_demo_2',
    kind: 'transcribe',
    label: 'Transcribe & redact',
    targetStage: 'redacted',
    limitN: null,
    concurrency: 2,
    total: 1968,
    processed: 214,
    failed: 3,
    status: 'interrupted',
    startedAt: '2026-08-09T23:10:02Z',
    finishedAt: '2026-08-10T04:52:39Z',
    itemCount: 217,
  },
  {
    id: 'job_demo_1',
    kind: 'transcribe',
    label: 'Transcribe & redact',
    targetStage: 'redacted',
    limitN: 4,
    concurrency: 2,
    total: 4,
    processed: 4,
    failed: 0,
    status: 'done',
    startedAt: '2026-08-08T14:22:00Z',
    finishedAt: '2026-08-08T14:29:31Z',
    itemCount: 4,
  },
]

function runItemsFor(runId: string, count: number): RunItem[] {
  const random = rng(runId.length * 7919)
  return Array.from({ length: count }, (_, i) => {
    // One failure and, for the interrupted run, a few calls that never finished.
    const failed = i === 3
    const interrupted = runId === 'job_demo_2' && i >= count - 3
    const status = failed ? 'failed' : interrupted ? 'interrupted' : 'done'
    return {
      runId,
      callId: `call_demo_${i + 1}`,
      name: CALLS[i % CALLS.length].displayName,
      stage: failed ? 'decoded' : interrupted ? 'transcribed' : 'redacted',
      status,
      secs: status === 'done' ? Math.round(90 + random() * 260) : null,
      error: failed ? 'decode failed: unsupported codec' : null,
      startedAt: '2026-08-10T21:04:11Z',
      updatedAt: '2026-08-10T21:31:48Z',
    } satisfies RunItem
  })
}

// ----------------------------------------------------------------- settings

const PROVIDERS: ProviderInfo[] = [
  ['nvidia_nim', 'NVIDIA NIM', 'NVIDIA_API_KEY', 'https://integrate.api.nvidia.com/v1', 'meta/llama-3.3-70b-instruct', 'environment'],
  ['openai', 'OpenAI', 'OPENAI_API_KEY', 'https://api.openai.com/v1', 'gpt-4o', 'missing'],
  ['openrouter', 'OpenRouter', 'OPENROUTER_API_KEY', 'https://openrouter.ai/api/v1', 'anthropic/claude-sonnet-4.5', 'missing'],
  ['groq', 'Groq', 'GROQ_API_KEY', 'https://api.groq.com/openai/v1', 'llama-3.3-70b-versatile', 'missing'],
  ['ollama', 'Ollama (local)', null, 'http://localhost:11434/v1', 'llama3.1', 'notRequired'],
  ['lmstudio', 'LM Studio (local)', null, 'http://localhost:1234/v1', 'local-model', 'notRequired'],
  ['openai_compatible', 'Custom (OpenAI-compatible)', 'CA_LLM_API_KEY', '', '', 'missing'],
  ['anthropic', 'Anthropic', 'ANTHROPIC_API_KEY', 'https://api.anthropic.com/v1', 'claude-sonnet-5', 'credentialStore'],
  ['gemini', 'Google Gemini', 'GEMINI_API_KEY', 'https://generativelanguage.googleapis.com/v1beta', 'gemini-2.5-pro', 'missing'],
].map(([id, label, envVar, endpoint, defaultModel, keySource]) => ({
  kind: id as string,
  id: id as string,
  label: label as string,
  isLocal: id === 'ollama' || id === 'lmstudio',
  requiresApiKey: id !== 'ollama' && id !== 'lmstudio',
  envVar: envVar as string | null,
  endpoint: endpoint as string,
  defaultModel: defaultModel as string,
  suggestedModels: defaultModel ? [defaultModel as string] : [],
  keySource: keySource as ProviderInfo['keySource'],
}))

let SETTINGS: AppSettings = {
  provider: 'nvidia_nim',
  model: 'meta/llama-3.3-70b-instruct',
  baseUrl: null,
  concurrency: 4,
  redactBeforeSend: true,
}

/** Agent output, in the shapes the real payloads use. */
const AGENT_RUNS = [
  {
    agent: 'executive_summary', label: 'Executive summary',
    payload: {
      summary: 'The customer was charged twice for order 4417. Priya identified the duplicate authorisation, released the reversal on the call and gave a reference number. The customer ended the call satisfied.',
      primaryIssue: 'Duplicate card charge',
      resolution: 'Reversal raised, reference RV-88214',
      customerOutcome: 'positive',
      risk: 'low',
      citations: [{ segmentId: 'seg_10', start: 54, end: 61.2, quote: 'reference RV-88214' }],
    },
    provider: 'nvidia-nim', model: 'meta/llama-3.3-70b-instruct',
    inputTokens: 2140, outputTokens: 180, costUsd: 0.0021, latencyMs: 3400,
    error: null, createdAt: '2026-08-11T09:00:00Z',
  },
  {
    agent: 'customer_experience', label: 'Customer experience',
    payload: {
      effort: 0.31, frustration: 0.23, resolutionConfidence: 0.94,
      escalationRisk: 0.07, churnRisk: 0.04,
      summary: 'Low effort: one contact, resolved on the call.',
      citations: [{ segmentId: 'seg_13', start: 86.2, end: 90.0, quote: 'quicker than I expected' }],
    },
    provider: 'nvidia-nim', model: 'meta/llama-3.3-70b-instruct',
    inputTokens: 2140, outputTokens: 96, costUsd: 0.0014, latencyMs: 2600,
    error: null, createdAt: '2026-08-11T09:00:00Z',
  },
  {
    agent: 'coaching', label: 'Coaching',
    payload: {
      summary: 'Strong resolution. One interruption and an unbounded hold.',
      strengths: [{
        behaviour: 'Acknowledged the problem unprompted',
        observation: 'Named the frustration before asking diagnostic questions.',
        recommendation: 'Keep opening with acknowledgement.',
        suggestedPhrase: null, severity: 'info',
        citations: [{ segmentId: 'seg_3', start: 13.1, end: 19.5, quote: "that's frustrating" }],
      }],
      improvements: [{
        behaviour: 'Hold was not time-boxed',
        observation: 'Announced a hold at 1:01 without saying how long it would take.',
        recommendation: 'State an expected duration before going on hold.',
        suggestedPhrase: 'Let me put you on hold for about two minutes while I confirm that.',
        severity: 'medium',
        citations: [{ segmentId: 'seg_11', start: 61.5, end: 66.2, quote: 'brief hold' }],
      }],
    },
    provider: 'nvidia-nim', model: 'meta/llama-3.3-70b-instruct',
    inputTokens: 2140, outputTokens: 240, costUsd: 0.0026, latencyMs: 4100,
    error: null, createdAt: '2026-08-11T09:00:00Z',
  },
  {
    agent: 'sentiment', label: 'Sentiment',
    payload: {
      overall: 0.42,
      summary:
        'The customer opens annoyed about the duplicate charge and warms once the reversal is confirmed. Nothing on the call pushes them back down.',
      // Deliberately crosses zero and ends higher than it starts: a trajectory
      // that only descends never exercises the sign handling in the plot.
      trajectory: [
        { t: 8.4, score: -0.55, label: 'annoyed at duplicate charge', speaker: 'customer',
          citations: [{ segmentId: 'seg_2', start: 8.4, end: 13, quote: 'charged twice' }] },
        { t: 31.2, score: -0.1, label: 'waiting on the check', speaker: 'customer',
          citations: [{ segmentId: 'seg_6', start: 31.2, end: 36, quote: 'how long will that take' }] },
        { t: 61.5, score: 0.05, label: 'neutral through the hold', speaker: 'customer',
          citations: [{ segmentId: 'seg_11', start: 61.5, end: 66.2, quote: 'brief hold' }] },
        { t: 78, score: 0.62, label: 'relieved at the reversal', speaker: 'customer',
          citations: [{ segmentId: 'seg_12', start: 78, end: 83, quote: 'that is a relief' }] },
        { t: 86.2, score: 0.71, label: 'pleased at the close', speaker: 'customer',
          citations: [{ segmentId: 'seg_13', start: 86.2, end: 90, quote: 'quicker than I expected' }] },
      ],
    },
    provider: 'nvidia-nim', model: 'meta/llama-3.3-70b-instruct',
    inputTokens: 2140, outputTokens: 210, costUsd: 0.0018, latencyMs: 2900,
    error: null, createdAt: '2026-08-11T09:00:00Z',
  },
  {
    agent: 'compliance', label: 'Compliance & risk',
    payload: {
      passed: false,
      summary:
        'Identification and disclosure were correct. One hold was placed without an estimate, which fails the call.',
      findings: FINDINGS,
    },
    provider: 'nvidia-nim', model: 'meta/llama-3.3-70b-instruct',
    inputTokens: 2140, outputTokens: 176, costUsd: 0.0019, latencyMs: 3100,
    error: null, createdAt: '2026-08-11T09:00:00Z',
  },
  {
    agent: 'root_cause', label: 'Root cause & intent',
    payload: {
      primaryCause: 'A retried card authorisation was captured twice by the payment gateway.',
      category: 'billing',
      confidence: 0.78,
      contributingFactors: [
        'The checkout retried automatically after a gateway timeout.',
        'No duplicate-capture check runs before settlement.',
        'The customer had no way to see the second charge as pending.',
      ],
      recommendedFix:
        'Add an idempotency key to the capture call so a retried authorisation cannot settle twice.',
      summary:
        'This is a payments-integration defect, not an agent error. The same retry path will produce the same call again.',
      citations: [{ segmentId: 'seg_2', start: 8.4, end: 13, quote: 'charged twice' }],
    },
    provider: 'nvidia-nim', model: 'meta/llama-3.3-70b-instruct',
    inputTokens: 2140, outputTokens: 205, costUsd: 0.002, latencyMs: 3300,
    error: null, createdAt: '2026-08-11T09:00:00Z',
  },
  {
    agent: 'action_items', label: 'Action items',
    payload: {
      summary: 'Three follow-ups, one of them owed to the customer.',
      items: [
        {
          title: 'Confirm the reversal has settled',
          detail: 'Check RV-88214 has cleared and email the customer confirmation.',
          owner: 'backoffice', priority: 'high', due: 'within 3 working days',
          citations: [{ segmentId: 'seg_10', start: 54, end: 61.2, quote: 'reference RV-88214' }],
        },
        {
          title: 'Raise a gateway ticket for the duplicate capture',
          detail: 'The retry path settled twice; payments engineering needs the authorisation ids.',
          owner: 'supervisor', priority: 'medium', due: '',
          citations: [{ segmentId: 'seg_2', start: 8.4, end: 13, quote: 'charged twice' }],
        },
        {
          title: 'Coach on time-boxing holds',
          detail: 'Cover the hold at 1:01 in the next one-to-one.',
          owner: 'supervisor', priority: 'low', due: 'next 1:1',
          citations: [{ segmentId: 'seg_11', start: 61.5, end: 66.2, quote: 'brief hold' }],
        },
      ],
    },
    provider: 'nvidia-nim', model: 'meta/llama-3.3-70b-instruct',
    inputTokens: 2140, outputTokens: 260, costUsd: 0.0024, latencyMs: 3600,
    error: null, createdAt: '2026-08-11T09:00:00Z',
  },
  {
    agent: 'qa_scorecard', label: 'QA scorecard',
    payload: SCORECARD,
    provider: 'nvidia-nim', model: 'meta/llama-3.3-70b-instruct',
    inputTokens: 2140, outputTokens: 420, costUsd: 0.0031, latencyMs: 5200,
    error: null, createdAt: '2026-08-11T09:00:00Z',
  },
  // Every real agent above renders through its own formatter. The two below are
  // the fallbacks, which no combination of the eight can produce: an agent this
  // build has no renderer for, and an agent that failed. Both states are easy to
  // ship broken precisely because a healthy fixture never shows them.
  {
    agent: 'rag_index', label: 'RAG index',
    payload: { indexed: true, chunks: 14, embeddingModel: 'nvidia/nv-embedqa-e5-v5' },
    provider: 'nvidia-nim', model: 'nvidia/nv-embedqa-e5-v5',
    inputTokens: 2140, outputTokens: 0, costUsd: 0.0002, latencyMs: 900,
    error: null, createdAt: '2026-08-11T09:00:00Z',
  },
  {
    agent: 'crm_sync', label: 'CRM sync',
    payload: null,
    provider: 'nvidia-nim', model: 'meta/llama-3.3-70b-instruct',
    inputTokens: 0, outputTokens: 0, costUsd: null, latencyMs: 180000,
    error: 'provider `nvidia-nim` transport error: timed out: operation timed out',
    createdAt: '2026-08-11T09:00:00Z',
  },
]

// ---------------------------------------------------------------------- live

/**
 * A scripted live call, replayed into the event stream.
 *
 * The live screen is the one surface with no static state to look at: every
 * pixel of it is driven by events arriving over time. Without this it can only
 * be checked by putting on a headset and talking, which is not something a
 * layout change should require.
 *
 * The script deliberately reaches every state the screen can be in — a blocking
 * alert, a monitor pass with a suggestion, a warning, the listening caret, and
 * the hand-off at the end.
 */
const LIVE_SCRIPT: { after: number; event: Record<string, unknown> }[] = [
  { after: 0, event: { kind: 'started', callId: 'call_live_demo', agentDevice: 'Headset Microphone', customerDevice: 'Headset Earphone (loopback)' } },
  { after: 400, event: { kind: 'speaking', channel: 'agent', speaking: true } },
  { after: 1400, event: { kind: 'speaking', channel: 'agent', speaking: false } },
  { after: 1500, event: { kind: 'stable', segmentId: 'lseg_1', channel: 'agent', startSecs: 0.4, endSecs: 4.1, text: 'Thanks for calling Northwind, my name is Priya. How can I help?' } },
  { after: 2200, event: { kind: 'speaking', channel: 'customer', speaking: true } },
  { after: 3600, event: { kind: 'speaking', channel: 'customer', speaking: false } },
  { after: 3700, event: { kind: 'stable', segmentId: 'lseg_2', channel: 'customer', startSecs: 5.2, endSecs: 11.8, text: "I've been charged twice for order 4417 and nobody has got back to me." } },
  { after: 4200, event: { kind: 'monitor', sentiment: -0.45, risk: 'medium', headline: 'Customer is frustrated about a duplicate charge and a missed callback.', sayNext: "I can see the duplicate charge — let me reverse it while you're on the line.", elapsedSecs: 12 } },
  { after: 5200, event: { kind: 'speaking', channel: 'agent', speaking: true } },
  { after: 6200, event: { kind: 'speaking', channel: 'agent', speaking: false } },
  { after: 6300, event: { kind: 'stable', segmentId: 'lseg_3', channel: 'agent', startSecs: 13.0, endSecs: 17.4, text: 'Let me put you on hold while I check that.' } },
  { after: 6500, event: { kind: 'rule', finding: { id: 'lfind_1', callId: 'call_live_demo', agent: 'rules', label: 'Live rules', code: 'HOLD_NOT_TIMEBOXED', title: 'Hold placed without a stated duration', detail: 'The agent went to hold without saying how long it would take.', severity: 'high', blocking: true, citations: [{ segmentId: 'lseg_3', start: 13.0, end: 17.4, quote: null }] } } },
  { after: 7400, event: { kind: 'stable', segmentId: 'lseg_4', channel: 'customer', startSecs: 18.0, endSecs: 24.6, text: 'Fine. The card ends 1111, it was four one one one one one one one one one one one one one one one.' } },
  { after: 7600, event: { kind: 'rule', finding: { id: 'lfind_2', callId: 'call_live_demo', agent: 'rules', label: 'Live rules', code: 'PCI_PAN_SPOKEN', title: 'Sensitive number spoken aloud', detail: 'A payment card number was said on the call by the customer. It is now in the recording.', severity: 'critical', blocking: true, citations: [{ segmentId: 'lseg_4', start: 18.0, end: 24.6, quote: null }] } } },
  { after: 8000, event: { kind: 'monitor', sentiment: -0.15, risk: 'critical', headline: 'A card number is now in the recording; the hold was not time-boxed.', sayNext: "I'm going to stop you there — please don't read the card number aloud.", elapsedSecs: 25 } },
  { after: 9000, event: { kind: 'warning', message: 'Live monitor unavailable: provider timed out' } },
  { after: 10000, event: { kind: 'monitor', sentiment: 0.35, risk: 'medium', headline: 'Reversal confirmed; the customer sounds satisfied.', sayNext: null, elapsedSecs: 40 } },
  { after: 10200, event: { kind: 'stable', segmentId: 'lseg_5', channel: 'customer', startSecs: 41.0, endSecs: 44.2, text: "Oh brilliant, that's a relief. Thank you." } },
  { after: 11500, event: { kind: 'ended', callId: 'call_live_demo', durationSecs: 46, mediaPath: 'C:/live/call_live_demo.wav' } },
]

/** The scripted call is 46 seconds of conversation replayed in ~12 seconds. */
const LIVE_DURATION_SECS = 46

let liveTimers: number[] = []
let liveMeter: number | null = null

function stopLiveMeter() {
  if (liveMeter !== null) clearInterval(liveMeter)
  liveMeter = null
}

function stopLiveScript() {
  for (const t of liveTimers) clearTimeout(t)
  liveTimers = []
  stopLiveMeter()
}

function playLiveScript() {
  stopLiveScript()
  for (const step of LIVE_SCRIPT) {
    liveTimers.push(
      setTimeout(() => {
        emit('live:event', { event: 'live:event', id: 0, payload: step.event })
      }, step.after) as unknown as number,
    )
  }
  // A level tick, so the meters and the clock are exercised rather than frozen.
  //
  // Stops when the scripted call ends. The real backend emits no Level events
  // after Ended, and a meter that kept running would leave the clock arguing
  // with the duration the session reported.
  let elapsed = 0
  liveMeter = setInterval(() => {
    elapsed += 0.5
    if (elapsed > LIVE_DURATION_SECS) {
      stopLiveMeter()
      return
    }
    emit('live:event', {
      event: 'live:event',
      id: 0,
      payload: {
        kind: 'level',
        // Deterministic wobble: a flat meter looks broken, a random one
        // differs between reloads and cannot be compared.
        agentDbfs: -50 + 25 * Math.abs(Math.sin(elapsed)),
        customerDbfs: -50 + 25 * Math.abs(Math.cos(elapsed * 0.8)),
        elapsedSecs: elapsed,
      },
    })
  }, 250) as unknown as number
}

// ------------------------------------------------------------------ dispatch

type Callback = (payload: unknown) => void

const callbacks = new Map<number, Callback>()
let nextCallbackId = 1
/** Event name → the callback ids currently listening to it. */
const listeners = new Map<string, Set<number>>()

function emit(event: string, payload: unknown) {
  for (const id of listeners.get(event) ?? []) callbacks.get(id)?.(payload)
}

/** Replay a short batch so the ingest screen's live queue can be seen working. */
function fakeBatch(limit: number | null) {
  const total = Math.min(limit ?? 6, 6)
  const stages = ['decoded', 'transcribed', 'diarized', 'redacted', 'analyzed']
  let delay = 220

  const send = (payload: unknown) => {
    setTimeout(() => emit('pipeline:progress', { event: 'pipeline:progress', id: 0, payload }), delay)
    delay += 180
  }

  send({ kind: 'discovered', callId: '', name: '', secs: total })
  for (let i = 0; i < total; i += 1) {
    const callId = `call_demo_${i + 1}`
    const name = CALLS[i]?.displayName ?? `demo-${i}.m4a`
    send({ kind: 'started', callId, name })
    // One call fails partway, so the queue's failure state is exercised — and
    // fails *at* a stage rather than after all of them, as a real one would.
    const failing = i === 4
    for (const stage of failing ? stages.slice(0, 1) : stages) {
      send({ kind: 'stage', callId, name, stage })
    }
    if (failing) send({ kind: 'failed', callId, name, error: 'decode failed: unsupported codec' })
    else send({ kind: 'finished', callId, name, secs: 96 + i * 37 })
  }
  send({ kind: 'done', callId: '', name: '', secs: total })
  setTimeout(() => emit('pipeline:runFinished', 'job_demo_3'), delay + 200)
}

async function dispatch(cmd: string, args: Record<string, unknown>): Promise<unknown> {
  // A visible delay, so loading states are actually exercised rather than
  // skipped over by an instant resolve.
  await new Promise((r) => setTimeout(r, 90))


// ---------------------------------------------------------------- people
//
// Shaped after the real corpus: one very busy contact whose calls have never
// been analysed, one agent with a full year of scores, and one contact with a
// single call. The unanalysed one is the important fixture — it is the state
// almost every person is in after a first ingest, and the screen has to read
// correctly without inventing numbers to fill the columns.

const PEOPLE: PersonSummary[] = [
  {
    id: 'person_busy',
    kind: 'contact',
    displayName: 'Ishita AI Cloud Koradia',
    handle: 'ishita ai cloud koradia',
    calls: 196,
    firstCall: '2024-12-14T09:12:00Z',
    lastCall: '2025-06-27T17:41:00Z',
  },
  {
    id: 'person_agent',
    kind: 'agent',
    displayName: 'Priya Sharma',
    handle: 'priya sharma',
    calls: 48,
    firstCall: '2025-01-08T09:00:00Z',
    lastCall: '2025-11-28T16:20:00Z',
  },
  {
    id: 'person_once',
    kind: 'contact',
    displayName: '+918485823280',
    handle: '+918485823280',
    calls: 1,
    firstCall: '2025-11-12T14:23:26Z',
    lastCall: '2025-11-12T14:23:26Z',
  },
]

function monthlySeries(base: number, spread: number, months: number): MonthPoint[] {
  const r = rng(9091)
  return Array.from({ length: months }, (_, i) => ({
    month: `2025-${String(i + 1).padStart(2, '0')}`,
    value: base + (r() - 0.5) * spread,
    calls: 2 + Math.floor(r() * 6),
  }))
}

function personDetail(personId: string): PersonDetail {
  const summary = PEOPLE.find((p) => p.id === personId) ?? PEOPLE[0]

  // The busy contact has never been analysed: every measure absent, every call
  // real. This is what most of a freshly ingested corpus looks like.
  if (summary.id === 'person_busy') {
    return {
      summary,
      facts: {
        personId: summary.id,
        calls: summary.calls,
        meanQa: null,
        meanSentiment: null,
        compliancePassRate: null,
        firstCall: summary.firstCall,
        lastCall: summary.lastCall,
      },
      qaTrend: [],
      sentimentTrend: [],
      repeatContactRate: 0.87,
      // Newest first, so the date and the sequence number move together —
      // a fixture where they disagree makes the real ordering look broken.
      calls: Array.from({ length: 12 }, (_, i) => ({
        callId: `call_busy_${i}`,
        occurredAt: `2025-${String(6 - Math.floor(i / 2)).padStart(2, '0')}-${String(27 - i).padStart(2, '0')}T11:00:00Z`,
        qaOverall: null,
        sentiment: null,
        sequence: 196 - i,
      })),
    }
  }

  if (summary.id === 'person_once') {
    return {
      summary,
      facts: {
        personId: summary.id,
        calls: 1,
        meanQa: 74,
        meanSentiment: 0.31,
        compliancePassRate: 1,
        firstCall: summary.firstCall,
        lastCall: summary.lastCall,
      },
      // One month is not a trend, and the screen must say so rather than draw
      // a single point as a line.
      qaTrend: [{ month: '2025-11', value: 74, calls: 1 }],
      sentimentTrend: [],
      repeatContactRate: 0,
      calls: [
        {
          callId: 'call_demo_1',
          occurredAt: summary.lastCall,
          qaOverall: 74,
          sentiment: 0.31,
          sequence: 1,
        },
      ],
    }
  }

  return {
    summary,
    facts: {
      personId: summary.id,
      calls: summary.calls,
      meanQa: 78.4,
      meanSentiment: 0.12,
      compliancePassRate: 0.83,
      firstCall: summary.firstCall,
      lastCall: summary.lastCall,
    },
    qaTrend: monthlySeries(78, 22, 11),
    sentimentTrend: monthlySeries(0.1, 0.7, 11),
    repeatContactRate: 0.24,
    calls: Array.from({ length: 10 }, (_, i) => ({
      callId: `call_agent_${i}`,
      occurredAt: `2025-${String(11 - i).padStart(2, '0')}-14T10:30:00Z`,
      qaOverall: i % 4 === 0 ? null : 64 + ((i * 7) % 30),
      sentiment: i % 3 === 0 ? null : (i % 5) / 10 - 0.2,
      sequence: 48 - i,
    })),
  }
}



  switch (cmd) {
    case 'get_startup':
      return STARTUP
    case 'get_stats':
      return STATS
    case 'list_calls': {
      const { offset = 0, limit = 50, search } = args as {
        offset?: number
        limit?: number
        search?: string | null
      }
      const term = (search ?? '').trim().toLowerCase()
      const matching = term
        ? CALLS.filter((c) => c.displayName.toLowerCase().includes(term))
        : CALLS
      return matching.slice(offset, offset + limit)
    }
    case 'get_call':
      return DETAIL
    case 'get_waveform':
      return makePeaks(DURATION)
    case 'get_scorecard':
      // Only the first call is scored, matching a corpus mid-run.
      return (args as { callId: string }).callId === 'call_demo_1' ? SCORECARD : null
    case 'discover_folder':
      return { total: 1968, added: 0 }
    case 'run_batch':
      fakeBatch((args as { limit: number | null }).limit)
      return null

    case 'get_settings':
      return SETTINGS
    case 'save_settings': {
      SETTINGS = (args as { settings: AppSettings }).settings
      return SETTINGS
    }
    case 'list_providers':
      return PROVIDERS
    case 'set_api_key': {
      const { provider } = args as { provider: string }
      const p = PROVIDERS.find((x) => x.id === provider)
      if (p) p.keySource = 'credentialStore'
      return PROVIDERS
    }
    case 'clear_api_key': {
      const { provider } = args as { provider: string }
      const p = PROVIDERS.find((x) => x.id === provider)
      if (p) p.keySource = p.requiresApiKey ? 'missing' : 'notRequired'
      return PROVIDERS
    }
    case 'test_provider':
      return {
        ok: true,
        model: SETTINGS.model,
        latencyMs: 412,
        message: `Answered in 412 ms as \`${SETTINGS.model}\` (fixture backend).`,
      }
    case 'run_scoring':
      fakeBatch(10)
      return null

    case 'ask_call': {
      const { question } = args as { question: string }
      // Grounded for most questions, deliberately ungrounded for one, so the
      // unverified state is visible in the fixture too.
      const ungrounded = /churn|predict|will they/i.test(question)
      return {
        answer: ungrounded
          ? 'Probably not, based on the tone at the end of the call.'
          : 'The customer was charged twice for order 4417. Priya confirmed two authorisations against the same card and raised a reversal on the call.',
        citations: ungrounded
          ? []
          : [
              { segmentId: 'seg_5', start: 24.2, end: 32.3, quote: 'two authorisations' },
              { segmentId: 'seg_10', start: 54, end: 61.2, quote: 'raised the reversal' },
            ],
        grounded: !ungrounded,
      }
    }

    case 'get_corpus_report':
      return {
        callsAnalysed: 214,
        meanQa: 86.7,
        qaDistribution: [
          { floor: 40, calls: 4 }, { floor: 50, calls: 9 }, { floor: 60, calls: 18 },
          { floor: 70, calls: 41 }, { floor: 80, calls: 82 }, { floor: 90, calls: 60 },
        ],
        meanEscalationRisk: 0.083,
        meanChurnRisk: 0.041,
        meanSentiment: 0.21,
        negativeShare: 0.187,
        compliancePassRate: 0.864,
        failingCalls: 29,
        topTopics: [
          { label: 'payment', calls: 96, share: 0.449 },
          { label: 'refund', calls: 61, share: 0.285 },
          { label: 'delivery', calls: 33, share: 0.154 },
          { label: 'account', calls: 27, share: 0.126 },
        ],
        complianceIssues: [
          { label: 'missing_disclosure', calls: 12, share: 0.056 },
          { label: 'unauthorised_commitment', calls: 3, share: 0.014 },
        ],
        topIssues: [
          { label: 'Payment failure', calls: 66, share: 0.308 },
          { label: 'Refund delay', calls: 30, share: 0.140 },
        ],
        rootCauseCategories: [
          { label: 'billing', calls: 98, share: 0.458 },
          { label: 'process', calls: 54, share: 0.252 },
          { label: 'product', calls: 38, share: 0.178 },
          { label: 'agent', calls: 15, share: 0.070 },
          { label: 'other', calls: 9, share: 0.042 },
        ],
        patterns: [
          { term: 'refund', outcome: 'escalation risk', withCalls: 61, withoutCalls: 153,
            withMean: 0.19, withoutMean: 0.07, lift: 2.7 },
          { term: 'resolved', outcome: 'QA score', withCalls: 88, withoutCalls: 126,
            withMean: 92.1, withoutMean: 81.4, lift: 1.13 },
        ],
      }

    case 'get_agent_runs':
      return AGENT_RUNS
    case 'get_findings': {
      const { callId } = args as { callId: string }
      return callId === 'call_demo_1' ? FINDINGS : []
    }
    case 'get_risk_register':
      return RISK_REGISTER

    case 'list_people': {
      const { kind, search } = args as { kind?: string | null; search?: string | null }
      const needle = (search ?? '').toLowerCase()
      return PEOPLE.filter(
        (p) =>
          (!kind || p.kind === kind) &&
          (!needle ||
            p.displayName.toLowerCase().includes(needle) ||
            p.handle.includes(needle)),
      )
    }
    case 'get_person': {
      const { personId } = args as { personId: string }
      return personDetail(personId)
    }
    case 'get_call_people':
      return {
        agentId: 'person_agent',
        agentName: 'Priya Sharma',
        contactId: 'person_busy',
        contactName: 'Ishita AI Cloud Koradia',
        attribution: 'rule',
        sequence: 5,
        contactCalls: 196,
      }
    case 'attribute_calls':
      return { scanned: 1972, attributed: 1972, skippedManual: 0, unmatched: 0 }
    case 'assign_call_person':
      return null

    case 'list_audio_devices':
      return {
        inputs: ['Headset Microphone (Jabra Evolve)', 'Microphone Array (Realtek)'],
        outputs: ['Headset Earphone (Jabra Evolve)', 'Speakers (Realtek)'],
        supported: true,
      }
    case 'start_live_session':
      playLiveScript()
      return { callId: 'call_live_demo', transcribing: true, advising: true }
    case 'stop_live_session':
      stopLiveScript()
      emit('live:event', {
        event: 'live:event',
        id: 0,
        payload: {
          kind: 'ended',
          callId: 'call_live_demo',
          durationSecs: 46,
          mediaPath: 'C:/live/call_live_demo.wav',
        },
      })
      return 'call_live_demo'
    case 'get_agent_health':
      return AGENT_RUNS.map((r) => ({
        agent: r.agent,
        label: r.label,
        runs: 4,
        failures: r.error ? 1 : 0,
        failureRate: r.error ? 0.25 : 0,
        inputTokens: r.inputTokens * 4,
        outputTokens: r.outputTokens * 4,
        costUsd: (r.costUsd ?? 0) * 4,
        meanLatencyMs: r.latencyMs,
        models: [r.model],
        lastError: r.error,
      }))

    case 'list_runs': {
      const { offset = 0, limit = 10 } = args as { offset?: number; limit?: number }
      return RUNS.slice(offset, offset + limit)
    }
    case 'get_run_items': {
      const { runId, offset = 0, limit = 50 } = args as {
        runId: string
        offset?: number
        limit?: number
      }
      const run = RUNS.find((r) => r.id === runId)
      if (!run) return []
      return runItemsFor(runId, run.itemCount).slice(offset, offset + limit)
    }

    case 'plugin:event|listen': {
      const { event, handler } = args as { event: string; handler: number }
      if (!listeners.has(event)) listeners.set(event, new Set())
      listeners.get(event)!.add(handler)
      return handler
    }
    case 'plugin:event|unlisten': {
      const { eventId } = args as { eventId: number }
      for (const set of listeners.values()) set.delete(eventId)
      return null
    }

    default:
      throw new Error(`mock backend has no command \`${cmd}\``)
  }
}

/**
 * Install the fake IPC layer. Call before rendering; no-op outside dev or when
 * a real Tauri runtime is present.
 */
export function installMockBackend() {
  if (!import.meta.env.DEV) return
  const w = window as unknown as Record<string, unknown>
  if (w.__TAURI_INTERNALS__) return

  w.__TAURI_INTERNALS__ = {
    invoke: (cmd: string, args: Record<string, unknown> = {}) => dispatch(cmd, args ?? {}),
    transformCallback: (cb: Callback) => {
      const id = nextCallbackId++
      callbacks.set(id, cb)
      return id
    },
    unregisterCallback: (id: number) => {
      callbacks.delete(id)
    },
    convertFileSrc: (path: string) => path,
  }

  // The event plugin unregisters through its own global, not through `invoke`.
  // Without this, every component unmount throws on teardown.
  w.__TAURI_EVENT_PLUGIN_INTERNALS__ = {
    unregisterListener: (event: string, eventId: number) => {
      listeners.get(event)?.delete(eventId)
      callbacks.delete(eventId)
    },
  }

  // eslint-disable-next-line no-console
  console.info('[dev] mock backend installed — data on screen is fixture data')
}
