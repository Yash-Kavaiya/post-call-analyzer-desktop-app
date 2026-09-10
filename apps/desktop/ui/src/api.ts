/**
 * Bridge to the Rust command layer.
 *
 * Every backend call goes through here so the rest of the UI never imports
 * Tauri directly — which keeps components testable in a plain browser and
 * means a future web build only has to replace this one file.
 *
 * Field names are camelCase on both sides, which the Rust structs achieve with
 * `#[serde(rename_all = "camelCase")]`. That was once asserted here and not
 * actually true of the audio and transcript types: the waveform arrived as
 * `min_max`, the renderer read `minMax`, and it silently drew nothing. The
 * guarantee is now enforced by tests in `ca-core` rather than by this comment.
 */

import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import type { Segment, WaveformPeaks } from './components/waveform'

export type { Segment, WaveformPeaks, Word, PeakLevel } from './components/waveform'

export interface CallSummary {
  id: string
  displayName: string
  stage: string
  durationSecs: number | null
  createdAt: string
  /** QA score 0..100, or null if the scorecard agent has not run. */
  overall: number | null
  /** Whole-call customer sentiment −1..1, or null if that agent has not run. */
  sentiment: number | null
  /** Null means unassessed, which the list shows differently from a pass. */
  compliancePassed: boolean | null
  /** Findings against the call severe enough to fail it. */
  blockingFindings: number
  speakerCount: number | null
  /** Who handled the call, once attributed. Null is normal for a recorder
   *  whose filenames name only the counterparty. */
  agentName: string | null
  contactName: string | null
  /** The real call time. Prefer this over createdAt, which is ingest time. */
  occurredAt: string | null
}

export interface Stats {
  totalCalls: number
  outstanding: number
  byStage: Record<string, number>
  inputTokens: number
  outputTokens: number
  costUsd: number
  audioSecs: number
}

export interface AcousticFeatures {
  meanPitchHz: number | null
  pitchStddevHz: number | null
  jitterPct: number | null
  meanRmsDb: number | null
  speechRateWpm: number | null
  silenceRatio: number | null
  crosstalkRatio: number | null
  longestPauseSecs: number | null
}

export interface Speaker {
  index: number
  role: 'agent' | 'customer' | 'unknown'
  roleConfidence: number | null
  totalSpeechSecs: number
  turnCount: number
}

export interface SpeakerStats {
  speakerIndex: number
  role: 'agent' | 'customer' | 'unknown'
  speechSecs: number
  /** Share of speaking time, 0..1 — silence excluded, so these sum to 1. */
  talkShare: number
  turns: number
  words: number
  wordsPerMinute: number
  questions: number
  longestTurnSecs: number
}

export interface Interruption {
  at: number
  bySpeaker: number
  overSpeaker: number
  overlapSecs: number
}

export interface Silence {
  start: number
  end: number
  secs: number
  afterSpeaker: number | null
}

export interface Monologue {
  speakerIndex: number
  start: number
  end: number
  secs: number
}

export interface KeywordHit {
  term: string
  count: number
  firstAt: number
}

/** Computed from the transcript, not from a model — exact and free. */
export interface ConversationMetrics {
  speakers: SpeakerStats[]
  interruptions: Interruption[]
  silences: Silence[]
  monologues: Monologue[]
  keywords: KeywordHit[]
  totalSilenceSecs: number
  silenceRatio: number
  longestSilenceSecs: number
  agentTalkShare: number | null
}

export interface CallDetail {
  id: string
  displayName: string
  durationSecs: number | null
  features: AcousticFeatures | null
  segments: Segment[]
  speakers: Speaker[]
  engine: string | null
  model: string | null
  mediaPath: string | null
  /** Talk ratio, interruptions, silences and keywords. Absent until transcribed. */
  metrics: ConversationMetrics | null
}

export interface Citation {
  segmentId: string
  start: number
  end: number
  quote: string | null
}

export interface CriterionScore {
  key: string
  label: string
  score: number
  weight: number
  reasoning: string
  citations: Citation[]
  notApplicable: boolean
}

export interface Scorecard {
  callId: string
  overall: number
  criteria: CriterionScore[]
  summary: string
  rubricId: string
  rubricVersion: string
}

export type Severity = 'info' | 'low' | 'medium' | 'high' | 'critical'

/**
 * One labelled moment on the sentiment trajectory.
 *
 * `t` comes from a validated citation rather than from a model-invented clock,
 * so seeking to it lands on the audio that produced the reading.
 */
export interface SentimentPoint {
  t: number
  /** −1 (very negative) .. 1 (very positive). */
  score: number
  label: string
  speaker: 'agent' | 'customer' | 'overall'
  citations: Citation[]
}

export interface SentimentReport {
  callId: string
  /** Whole-call customer sentiment, −1..1. */
  overall: number
  trajectory: SentimentPoint[]
  summary: string
}

/**
 * A compliance, risk or friction observation.
 *
 * The same shape whether it arrives inside a `ComplianceReport` payload or from
 * the durable ledger via {@link getFindings} — the ledger adds `label` and
 * `blocking`.
 */
export interface Finding {
  id: string
  callId: string
  agent: string
  /** Display name of the agent that raised it. Ledger reads only. */
  label?: string
  code: string
  title: string
  detail: string
  severity: Severity
  /** Whether this severity alone fails the call. Ledger reads only. */
  blocking?: boolean
  citations: Citation[]
}

export interface ComplianceReport {
  callId: string
  /** Recomputed from surviving findings, never taken from the model. */
  passed: boolean
  findings: Finding[]
  summary: string
}

export interface RootCauseReport {
  callId: string
  primaryCause: string
  /** billing | product | process | agent | other. */
  category: string
  confidence: number
  contributingFactors: string[]
  recommendedFix: string
  citations: Citation[]
  summary: string
}

export interface ActionItem {
  title: string
  detail: string
  /** agent | supervisor | backoffice | customer | system. */
  owner: string
  priority: 'low' | 'medium' | 'high'
  /** Free-text due hint from the conversation. May be empty. */
  due: string
  citations: Citation[]
}

export interface ActionItemsReport {
  callId: string
  items: ActionItem[]
  summary: string
}

/** One rule, and how much of the corpus breaks it. */
export interface FindingCount {
  code: string
  title: string
  severity: Severity
  /** Distinct calls, not rows. */
  calls: number
  blocking: boolean
}

/** What the backend resolved at launch — surfaced so a missing model warns clearly. */
export interface Startup {
  whisperBinary: string
  whisperBinaryPresent: boolean
  whisperModel: string
  whisperModelPresent: boolean
  englishOnly: boolean
}

/** A batch, as the history list shows it. */
export interface RunSummary {
  id: string
  kind: string
  label: string
  targetStage: string
  limitN: number | null
  concurrency: number
  /** What the run set out to do. Differs from `itemCount` when interrupted. */
  total: number
  processed: number
  failed: number
  status: 'running' | 'done' | 'interrupted'
  startedAt: string
  finishedAt: string | null
  /** Rows actually recorded, which is what the item list pages through. */
  itemCount: number
}

/** One call's outcome inside one run. */
export interface RunItem {
  runId: string
  callId: string
  name: string
  stage: string
  status: 'running' | 'done' | 'failed' | 'interrupted'
  secs: number | null
  error: string | null
  startedAt: string
  updatedAt: string
}

/** Where a provider's key is coming from. Never the key itself. */
export type KeySource = 'credentialStore' | 'environment' | 'notRequired' | 'missing'

/** One vendor, as the settings screen shows it. */
export interface ProviderInfo {
  kind: string
  id: string
  label: string
  isLocal: boolean
  requiresApiKey: boolean
  /** The vendor's conventional variable, as an alternative to typing a key. */
  envVar: string | null
  endpoint: string
  defaultModel: string
  suggestedModels: string[]
  keySource: KeySource
}

export interface AppSettings {
  provider: string
  model: string
  baseUrl: string | null
  concurrency: number
  redactBeforeSend: boolean
}

export interface ProviderTest {
  ok: boolean
  model: string | null
  latencyMs: number | null
  message: string
}

export type CloudProviderId = 's3' | 'azure' | 'gcs'

export interface CloudProviderInfo {
  id: CloudProviderId
  label: string
}

/** What to scan in a public bucket. */
export interface CloudSpec {
  provider: CloudProviderId
  bucket: string
  prefix: string
  region: string | null
  account: string | null
  endpoint: string | null
  includeTranscripts: boolean
}

/** One agent's run against one call, with its accounting. */
export interface AgentRun {
  agent: string
  label: string
  /** The agent's output. Null when the run failed. */
  payload: unknown
  provider: string
  /** The model that actually answered, which is not always the one asked for. */
  model: string
  inputTokens: number
  outputTokens: number
  costUsd: number | null
  latencyMs: number
  error: string | null
  createdAt: string
}

/** Health and spend per agent across the corpus. */
export interface AgentHealth {
  agent: string
  label: string
  runs: number
  failures: number
  failureRate: number
  inputTokens: number
  outputTokens: number
  costUsd: number
  meanLatencyMs: number
  models: string[]
  lastError: string | null
}

export interface CopilotTurn {
  role: 'user' | 'assistant'
  content: string
}

export interface CopilotAnswer {
  answer: string
  citations: Citation[]
  /** False when nothing the model cited resolved to the transcript. */
  grounded: boolean
}

export interface Counted {
  label: string
  calls: number
  /** Share of analysed calls, 0..1. */
  share: number
}

export interface ScoreBucket {
  /** Inclusive lower bound of a ten-point band. */
  floor: number
  calls: number
}

/**
 * An association between a term and an outcome — not a cause.
 *
 * Both group sizes are carried so a ratio is never shown without its
 * denominator.
 */
export interface Pattern {
  term: string
  outcome: string
  withCalls: number
  withoutCalls: number
  withMean: number
  withoutMean: number
  /** `withMean / withoutMean`. Above 1 is higher, below 1 is lower. */
  lift: number
}

export interface CorpusReport {
  callsAnalysed: number
  meanQa: number | null
  qaDistribution: ScoreBucket[]
  meanEscalationRisk: number | null
  meanChurnRisk: number | null
  /** Mean whole-call customer sentiment, −1..1. */
  meanSentiment: number | null
  /** Share of the calls *with* a sentiment score where the customer left unhappy. */
  negativeShare: number | null
  /** Share of the calls the compliance agent judged that it passed. */
  compliancePassRate: number | null
  /** Calls with at least one finding severe enough to fail them. */
  failingCalls: number
  topTopics: Counted[]
  complianceIssues: Counted[]
  topIssues: Counted[]
  /** Root causes rolled up into billing / product / process / agent / other. */
  rootCauseCategories: Counted[]
  patterns: Pattern[]
}

export interface ProgressEvent {
  kind: 'discovered' | 'started' | 'stage' | 'finished' | 'failed' | 'done'
  callId: string
  name: string
  stage?: string | null
  secs?: number | null
  error?: string | null
}

export const getStartup = (): Promise<Startup> => invoke('get_startup')

export const getStats = (): Promise<Stats> => invoke('get_stats')

export const listCalls = (
  offset: number,
  limit: number,
  search?: string,
): Promise<CallSummary[]> => invoke('list_calls', { offset, limit, search: search ?? null })

export const getCall = (callId: string): Promise<CallDetail> => invoke('get_call', { callId })

export const getWaveform = (callId: string): Promise<WaveformPeaks | null> =>
  invoke('get_waveform', { callId })

export const getScorecard = (callId: string): Promise<Scorecard | null> =>
  invoke('get_scorecard', { callId })

export const discoverFolder = (path: string): Promise<{ total: number; added: number }> =>
  invoke('discover_folder', { path })

/**
 * Starts a batch and returns immediately — a run can last hours, so progress
 * arrives via {@link onPipelineProgress} rather than by awaiting this.
 */
export const runBatch = (limit: number | null, concurrency: number): Promise<void> =>
  invoke('run_batch', { limit, concurrency })

/** Register a single file or a folder — the backend handles either. */
export const discoverPath = (path: string): Promise<{ total: number; added: number }> =>
  invoke('discover_path', { path })

export const listCloudProviders = (): Promise<CloudProviderInfo[]> =>
  invoke('list_cloud_providers')

/** Scan a public bucket. Lists only; objects download when a batch reaches them. */
export const discoverCloud = (spec: CloudSpec): Promise<{ total: number; added: number }> =>
  invoke('discover_cloud', { spec })

/** Take one call through transcription and, if configured, analysis. */
export const processCall = (callId: string, analyze: boolean): Promise<void> =>
  invoke('process_call', { callId, analyze })

/** Ask a question about one call, answered from its transcript. */
export const askCall = (
  callId: string,
  question: string,
  history: CopilotTurn[],
): Promise<CopilotAnswer> => invoke('ask_call', { callId, question, history })

export const getAgentRuns = (callId: string): Promise<AgentRun[]> =>
  invoke('get_agent_runs', { callId })

export const getAgentHealth = (): Promise<AgentHealth[]> => invoke('get_agent_health')

/**
 * The durable compliance ledger for one call, worst finding first.
 *
 * Read separately from the compliance agent's payload because the ledger
 * outlives any one run of the agent and is what the risk register counts.
 */
export const getFindings = (callId: string): Promise<Finding[]> =>
  invoke('get_findings', { callId })

/** Which rules the corpus breaks, and how many calls break each. */
export const getRiskRegister = (): Promise<FindingCount[]> => invoke('get_risk_register')

/** Aggregate view across every analysed call. */
export const getCorpusReport = (): Promise<CorpusReport> => invoke('get_corpus_report')

export const getSettings = (): Promise<AppSettings> => invoke('get_settings')

export const saveSettings = (settings: AppSettings): Promise<AppSettings> =>
  invoke('save_settings', { settings })

export const listProviders = (): Promise<ProviderInfo[]> => invoke('list_providers')

/**
 * Hand a key to the OS credential store. It is not stored in the app database
 * and is never returned; the reply is the refreshed catalogue, in which the
 * provider's `keySource` has changed.
 */
export const setApiKey = (provider: string, key: string): Promise<ProviderInfo[]> =>
  invoke('set_api_key', { provider, key })

export const clearApiKey = (provider: string): Promise<ProviderInfo[]> =>
  invoke('clear_api_key', { provider })

export const testProvider = (settings: AppSettings): Promise<ProviderTest> =>
  invoke('test_provider', { settings })

/** Score calls with the configured provider. Progress arrives as usual. */
export const runScoring = (limit: number | null): Promise<void> =>
  invoke('run_scoring', { limit })

export const listRuns = (offset: number, limit: number): Promise<RunSummary[]> =>
  invoke('list_runs', { offset, limit })

export const getRunItems = (
  runId: string,
  offset: number,
  limit: number,
): Promise<RunItem[]> => invoke('get_run_items', { runId, offset, limit })

/** Subscribe to batch progress. Returns an unsubscribe function. */
export function onPipelineProgress(cb: (e: ProgressEvent) => void): () => void {
  const pending = listen<ProgressEvent>('pipeline:progress', (event) => cb(event.payload))
  let disposed = false
  let stop: (() => void) | null = null

  pending.then((unlisten) => {
    // The component may unmount before `listen` resolves; honour that.
    if (disposed) unlisten()
    else stop = unlisten
  })

  return () => {
    disposed = true
    stop?.()
  }
}

/** Fires when a batch finishes, so the history can refresh itself. */
export function onRunFinished(cb: (runId: string) => void): () => void {
  const pending = listen<string>('pipeline:runFinished', (event) => cb(event.payload))
  let disposed = false
  let stop: (() => void) | null = null

  pending.then((unlisten) => {
    if (disposed) unlisten()
    else stop = unlisten
  })

  return () => {
    disposed = true
    stop?.()
  }
}

/** Convert a filesystem path into a URL the <audio> element can load. */
export async function mediaUrl(path: string): Promise<string> {
  const { convertFileSrc } = await import('@tauri-apps/api/core')
  return convertFileSrc(path)
}

export function formatDuration(secs: number | null | undefined): string {
  if (secs == null || !Number.isFinite(secs)) return '—'
  const m = Math.floor(secs / 60)
  const s = Math.floor(secs % 60)
  return `${m}:${String(s).padStart(2, '0')}`
}

/**
 * The date part of a timestamp, or a dash.
 *
 * A call with no `occurredAt` has never been attributed a time; showing it as
 * an epoch date or as blank would both be worse than saying so.
 */
export function day(iso: string | null | undefined): string {
  return iso ? iso.slice(0, 10) : '—'
}

export function formatHours(secs: number): string {
  if (secs < 3600) return `${Math.round(secs / 60)} min`
  return `${(secs / 3600).toFixed(1)} h`
}

/**
 * A timestamp a person can read at a glance, with the exact value kept for the
 * `title` attribute. Recent work is what gets scanned, so recency is what the
 * short form optimises for.
 */
export function formatWhen(iso: string): string {
  const then = new Date(iso)
  if (Number.isNaN(then.getTime())) return iso

  const secs = (Date.now() - then.getTime()) / 1000
  if (secs < 60) return 'just now'
  if (secs < 3600) return `${Math.floor(secs / 60)} min ago`
  if (secs < 86400) return `${Math.floor(secs / 3600)} h ago`
  if (secs < 604800) return `${Math.floor(secs / 86400)} d ago`
  return then.toLocaleDateString(undefined, { day: 'numeric', month: 'short' })
}

/** Wall-clock length of a run, or null while it is still going. */
export function runDuration(startedAt: string, finishedAt: string | null): string | null {
  if (!finishedAt) return null
  const ms = new Date(finishedAt).getTime() - new Date(startedAt).getTime()
  if (!Number.isFinite(ms) || ms < 0) return null
  const secs = Math.round(ms / 1000)
  if (secs < 60) return `${secs}s`
  const mins = Math.floor(secs / 60)
  if (mins < 60) return `${mins}m ${secs % 60}s`
  return `${Math.floor(mins / 60)}h ${mins % 60}m`
}

// ------------------------------------------------------------------- live

/** Which capture device an utterance arrived on. */
export type LiveChannel = 'agent' | 'customer'

export interface AudioDevices {
  inputs: string[]
  outputs: string[]
  /** False when this build cannot capture at all. */
  supported: boolean
}

export interface LiveStarted {
  callId: string
  /** False still records and still runs the offline rules — only the live
   *  transcript is missing, and the batch pass produces the real one. */
  transcribing: boolean
  advising: boolean
}

/**
 * What the live screen is told, as it happens.
 *
 * Note the absence of a partial-text variant. Chunks are cut at pauses and
 * transcribed whole, so text arrives already final and is never revised;
 * `speaking` drives the listening marker instead.
 */
export type LiveEvent =
  | { kind: 'started'; callId: string; agentDevice: string; customerDevice: string }
  | { kind: 'level'; agentDbfs: number; customerDbfs: number; elapsedSecs: number }
  | { kind: 'speaking'; channel: LiveChannel; speaking: boolean }
  | {
      kind: 'stable'
      segmentId: string
      channel: LiveChannel
      startSecs: number
      endSecs: number
      text: string
    }
  | { kind: 'rule'; finding: Finding }
  | {
      kind: 'monitor'
      sentiment: number
      risk: Severity
      headline: string
      sayNext: string | null
      elapsedSecs: number
    }
  | { kind: 'warning'; message: string }
  | { kind: 'ended'; callId: string; durationSecs: number; mediaPath: string }

export const listAudioDevices = (): Promise<AudioDevices> => invoke('list_audio_devices')

/** Begin capturing. Returns once the session is running, not when it ends. */
export const startLiveSession = (
  inputDevice: string | null,
  outputDevice: string | null,
  replayPath?: string | null,
): Promise<LiveStarted> =>
  invoke('start_live_session', {
    inputDevice,
    outputDevice,
    replayPath: replayPath ?? null,
  })

/** Stop capturing. The battery then runs in the background. */
export const stopLiveSession = (): Promise<string | null> => invoke('stop_live_session')

/** Subscribe to live session events. Returns an unsubscribe function. */
export function onLiveEvent(cb: (e: LiveEvent) => void): () => void {
  const pending = listen<LiveEvent>('live:event', (event) => cb(event.payload))
  let disposed = false
  let stop: (() => void) | null = null

  pending.then((unlisten) => {
    if (disposed) unlisten()
    else stop = unlisten
  })

  return () => {
    disposed = true
    stop?.()
  }
}

// ------------------------------------------------------------- participants

export interface PersonSummary {
  id: string
  kind: 'agent' | 'contact'
  displayName: string
  handle: string
  calls: number
  /** Null when no call of theirs carries a time — not the same as no calls. */
  firstCall: string | null
  lastCall: string | null
  /** Meaningless for a contact; agents default to the narrowest role. */
  role: 'agent' | 'supervisor' | 'admin'
  team: string | null
}

export interface MonthPoint {
  month: string
  value: number
  calls: number
}

export interface PersonFacts {
  personId: string
  calls: number
  /**
   * Null means no analysed call, which is not the same as a score of zero.
   * Rendering null as 0 would make a person nobody has scored look identical
   * to the worst agent on the floor.
   */
  meanQa: number | null
  meanSentiment: number | null
  compliancePassRate: number | null
  firstCall: string | null
  lastCall: string | null
}

export interface PersonCall {
  callId: string
  occurredAt: string | null
  qaOverall: number | null
  sentiment: number | null
  /** Position in this contact's run of calls, oldest first. 1 is a first contact. */
  sequence: number
}

export interface PersonDetail {
  summary: PersonSummary
  facts: PersonFacts
  qaTrend: MonthPoint[]
  sentimentTrend: MonthPoint[]
  /** Null when no call carries a time: unanswerable rather than zero. */
  repeatContactRate: number | null
  calls: PersonCall[]
}

export interface CallPeople {
  agentId: string | null
  agentName: string | null
  contactId: string | null
  contactName: string | null
  attribution: 'none' | 'rule' | 'manual' | 'live'
  /** Position in the contact's history, oldest first. 1 is a first contact. */
  sequence: number | null
  contactCalls: number | null
}

export interface AttributeReport {
  scanned: number
  attributed: number
  skippedManual: number
  unmatched: number
}

export const listPeople = (kind?: string, search?: string): Promise<PersonSummary[]> =>
  invoke('list_people', { kind: kind ?? null, search: search ?? null })

export const getPerson = (personId: string): Promise<PersonDetail> =>
  invoke('get_person', { personId })

export const attributeCalls = (dryRun: boolean, force: boolean): Promise<AttributeReport> =>
  invoke('attribute_calls', { dryRun, force })

export interface PluginAnalysis {
  plugin: string
  /** Declared per plugin, so the shape is not known ahead of time. */
  payload: Record<string, unknown> | null
  provider: string
  model: string
  costUsd: number | null
  createdAt: string
  /** Set when the plugin ran but its output failed validation. */
  error: string | null
}

export const getPluginAnalyses = (callId: string): Promise<PluginAnalysis[]> =>
  invoke('get_plugin_analyses', { callId })

export interface SyncSettings {
  url: string | null
  /** Whether a token is stored. The value never crosses this boundary. */
  hasToken: boolean
}

export interface SyncOutcome {
  pushed: number
  pulled: number
  /** Records the local guard refused to send. */
  withheld: number
  rejected: number
}

export interface AgentRollup {
  agentId: string
  calls: number
  /** Null when none of their calls carry the measure. */
  meanQa: number | null
  meanSentiment: number | null
  compliancePassRate: number | null
  failingCalls: number
}

export interface CodeRollup {
  code: string
  severity: string
  /** Calls that raised it, not raw occurrences. */
  calls: number
}

export interface MonthRollup {
  month: string
  calls: number
  meanQa: number | null
}

export interface Rollup {
  calls: number
  callsWithoutATime: number
  meanQa: number | null
  meanSentiment: number | null
  compliancePassRate: number | null
  /** How many calls the mean covers. */
  scoredCalls: number
  agents: AgentRollup[]
  codes: CodeRollup[]
  months: MonthRollup[]
}

export const getSyncSettings = (): Promise<SyncSettings> => invoke('get_sync_settings')

/** An undefined token leaves the stored one alone; an empty string clears it. */
export const saveSyncSettings = (
  url: string | null,
  token?: string,
): Promise<SyncSettings> => invoke('save_sync_settings', { url, token })

export const runSync = (): Promise<SyncOutcome> => invoke('run_sync')

export const getSyncReport = (): Promise<Rollup> => invoke('get_sync_report')

export interface PluginInfo {
  name: string | null
  title: string | null
  path: string
  findings: boolean
  error: string | null
}

export interface PluginsSnapshot {
  /** Directory this install reads .toml files from. */
  dir: string
  plugins: PluginInfo[]
}

export interface PluginRunOutcome {
  ran: number
  skipped: number
  failed: number
}

export const listPlugins = (): Promise<PluginsSnapshot> => invoke('list_plugins')

export const runPlugins = (callId: string, force: boolean): Promise<PluginRunOutcome> =>
  invoke('run_plugins', { callId, force })

export interface ViewScopeInfo {
  handle: string | null
  role: 'agent' | 'supervisor' | 'admin'
  /** True when nothing is filtered: no operator set, or an admin. */
  unrestricted: boolean
  team: string | null
}

export interface TeamSummary {
  id: string
  name: string
  members: number
}

export const getViewScope = (): Promise<ViewScopeInfo> => invoke('get_view_scope')

export const setOperator = (handle: string | null): Promise<void> =>
  invoke('set_operator', { handle })

export const listTeams = (): Promise<TeamSummary[]> => invoke('list_teams')

export const setPersonRole = (personId: string, role: string): Promise<void> =>
  invoke('set_person_role', { personId, role })

export const setPersonTeam = (personId: string, team: string | null): Promise<void> =>
  invoke('set_person_team', { personId, team })

export interface ResetReport {
  calls: number
  transcripts: number
  analyses: number
  findings: number
  people: number
  runs: number
}

export interface ResetOutcome {
  report: ResetReport
  /** Where the copy went, so a mistaken reset can be recovered from a file. */
  backupPath: string | null
}

export const previewReset = (scope: string): Promise<ResetReport> =>
  invoke('preview_reset', { scope })

export const resetData = (scope: string, backup: boolean): Promise<ResetOutcome> =>
  invoke('reset_data', { scope, backup })

export const getCallPeople = (callId: string): Promise<CallPeople> =>
  invoke('get_call_people', { callId })

export const assignCallPerson = (
  callId: string,
  agentName: string | null,
  contactName: string | null,
): Promise<void> => invoke('assign_call_person', { callId, agentName, contactName })
