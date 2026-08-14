/**
 * Settings: which model runs the analysis, on what terms, with whose key.
 *
 * Three things this screen is careful about:
 *
 * - **The key is write-only.** It is sent once and never read back. The field
 *   clears on save and the screen afterwards states only that a key exists and
 *   where it came from — a form that repopulates a secret is a form that leaks
 *   it into screenshots and bug reports.
 * - **The endpoint is shown.** Before anything is sent anywhere, the host that
 *   will receive it is on screen.
 * - **It can be tested.** A wrong key should cost one button press to discover,
 *   not three hours of a metered batch.
 */

import { useCallback, useEffect, useMemo, useState } from 'react'
import {
  clearApiKey,
  getSettings,
  listProviders,
  runScoring,
  saveSettings,
  setApiKey,
  testProvider,
  type AppSettings,
  type KeySource,
  type ProviderInfo,
  type ProviderTest,
} from '../api'
import { ResetPanel } from '../components/settings/ResetPanel'
import { ScopePanel } from '../components/settings/ScopePanel'
import './SettingsScreen.css'

const KEY_SOURCE_LABEL: Record<KeySource, string> = {
  credentialStore: 'Key stored in Windows Credential Manager',
  environment: 'Using the key from your environment',
  notRequired: 'Runs on this machine — no key needed',
  missing: 'No key yet',
}

export function SettingsScreen() {
  const [providers, setProviders] = useState<ProviderInfo[]>([])
  const [settings, setSettings] = useState<AppSettings | null>(null)
  const [keyInput, setKeyInput] = useState('')
  const [saved, setSaved] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [test, setTest] = useState<ProviderTest | null>(null)
  const [busy, setBusy] = useState<'save' | 'test' | 'key' | 'score' | null>(null)

  useEffect(() => {
    Promise.all([getSettings(), listProviders()])
      .then(([s, p]) => {
        setSettings(s)
        setProviders(p)
      })
      .catch((e: unknown) => setError(String(e)))
  }, [])

  const current = useMemo(
    () => providers.find((p) => p.id === settings?.provider) ?? null,
    [providers, settings?.provider],
  )

  const update = useCallback((patch: Partial<AppSettings>) => {
    setSettings((prev) => (prev ? { ...prev, ...patch } : prev))
    setSaved(null)
    setTest(null)
  }, [])

  /** Switching vendor carries the old vendor's model name with it otherwise. */
  const changeProvider = useCallback(
    (id: string) => {
      const p = providers.find((x) => x.id === id)
      update({
        provider: id,
        model: p?.defaultModel ?? '',
        baseUrl: id === 'openai_compatible' ? (settings?.baseUrl ?? '') : null,
      })
      setKeyInput('')
    },
    [providers, settings?.baseUrl, update],
  )

  const doSave = useCallback(async () => {
    if (!settings) return
    setBusy('save')
    setError(null)
    try {
      const stored = await saveSettings(settings)
      setSettings(stored)
      setSaved('Settings saved.')
    } catch (e) {
      setError(String(e))
    } finally {
      setBusy(null)
    }
  }, [settings])

  const doSaveKey = useCallback(async () => {
    if (!settings || keyInput.trim() === '') return
    setBusy('key')
    setError(null)
    try {
      setProviders(await setApiKey(settings.provider, keyInput))
      // Cleared immediately: the value has done its job and should not sit in
      // a DOM node for the rest of the session.
      setKeyInput('')
      setSaved('Key saved to Windows Credential Manager.')
    } catch (e) {
      setError(String(e))
    } finally {
      setBusy(null)
    }
  }, [settings, keyInput])

  const doClearKey = useCallback(async () => {
    if (!settings) return
    setBusy('key')
    try {
      setProviders(await clearApiKey(settings.provider))
      setSaved('Key removed.')
    } catch (e) {
      setError(String(e))
    } finally {
      setBusy(null)
    }
  }, [settings])

  const doTest = useCallback(async () => {
    if (!settings) return
    setBusy('test')
    setError(null)
    setTest(null)
    try {
      setTest(await testProvider(settings))
    } catch (e) {
      setError(String(e))
    } finally {
      setBusy(null)
    }
  }, [settings])

  const doScore = useCallback(async () => {
    setBusy('score')
    setError(null)
    setSaved(null)
    try {
      await runScoring(10)
      setSaved('Scoring started — watch it on the Ingest screen.')
    } catch (e) {
      setError(String(e))
    } finally {
      setBusy(null)
    }
  }, [])

  if (error && !settings) return <p className="set-error">Could not load settings: {error}</p>
  if (!settings) return <p className="set-state">Loading…</p>

  const needsKey = current?.requiresApiKey ?? true
  const hasKey = current ? current.keySource !== 'missing' : false
  const isCustom = settings.provider === 'openai_compatible'

  return (
    <div className="set-root">
      <h2>Settings</h2>

      <section className="set-card">
        <h3>Analysis model</h3>
        <p className="set-hint">
          Which model the QA scorecard and future agents talk to. Everything here applies
          to analysis only — transcription runs locally either way.
        </p>

        <div className="set-grid">
          <label className="set-field">
            <span>Provider</span>
            <select
              value={settings.provider}
              onChange={(e) => changeProvider(e.target.value)}
            >
              {providers.map((p) => (
                <option key={p.id} value={p.id}>
                  {p.label}
                </option>
              ))}
            </select>
          </label>

          <label className="set-field">
            <span>Model</span>
            <input
              list="set-model-options"
              value={settings.model}
              onChange={(e) => update({ model: e.target.value })}
              placeholder={current?.defaultModel ?? 'model name'}
              spellCheck={false}
            />
            {/* Free text with suggestions: vendors add models faster than any
                hardcoded list can follow, so the list must never be a ceiling. */}
            <datalist id="set-model-options">
              {(current?.suggestedModels ?? []).map((m) => (
                <option key={m} value={m} />
              ))}
            </datalist>
          </label>
        </div>

        {isCustom ? (
          <label className="set-field set-wide">
            <span>Base URL</span>
            <input
              value={settings.baseUrl ?? ''}
              onChange={(e) => update({ baseUrl: e.target.value })}
              placeholder="https://your-gateway.example.com/v1"
              spellCheck={false}
            />
          </label>
        ) : (
          <p className="set-endpoint">
            Requests go to <code>{current?.endpoint}</code>
          </p>
        )}
      </section>

      <section className="set-card">
        <h3>API key</h3>
        <p className={`set-keystate set-keystate-${current?.keySource ?? 'missing'}`}>
          {KEY_SOURCE_LABEL[current?.keySource ?? 'missing']}
          {current?.envVar && current.keySource === 'missing' && (
            <> — or set <code>{current.envVar}</code> in your environment.</>
          )}
        </p>

        {needsKey && (
          <>
            <div className="set-row">
              <input
                className="set-key"
                type="password"
                value={keyInput}
                onChange={(e) => setKeyInput(e.target.value)}
                placeholder={hasKey ? 'Replace the stored key…' : 'Paste your API key'}
                autoComplete="off"
                spellCheck={false}
              />
              <button type="button" onClick={doSaveKey} disabled={busy !== null || !keyInput.trim()}>
                {busy === 'key' ? 'Saving…' : 'Save key'}
              </button>
              {current?.keySource === 'credentialStore' && (
                <button type="button" className="set-ghost" onClick={doClearKey} disabled={busy !== null}>
                  Remove
                </button>
              )}
            </div>
            <p className="set-hint">
              Stored in Windows Credential Manager under your user account — not in the
              app database, and never shown again once saved.
            </p>
          </>
        )}
      </section>

      <section className="set-card">
        <h3>How analysis runs</h3>
        <div className="set-grid">
          <label className="set-field">
            <span>Concurrent model calls</span>
            <input
              type="number"
              min={1}
              max={32}
              value={settings.concurrency}
              onChange={(e) => update({ concurrency: Number(e.target.value) || 1 })}
            />
          </label>
          {current?.isLocal ? (
            <label className="set-check">
              <input
                type="checkbox"
                checked={settings.redactBeforeSend}
                onChange={(e) => update({ redactBeforeSend: e.target.checked })}
              />
              <span>
                Redact personal data before sending
                <em> — optional here, since {current.label} runs on this machine.</em>
              </span>
            </label>
          ) : (
            /* Deliberately not a disabled checkbox. A greyed-out box reads as
               "off" at a glance, and this is the one setting that must never be
               misread: it is what stops real phone numbers reaching a vendor. */
            <p className="set-locked">
              <span className="set-lock-chip">Always on</span>
              Personal data is redacted before anything is sent to{' '}
              {current?.label ?? 'a remote provider'}.
            </p>
          )}
        </div>
      </section>

      <div className="set-actions">
        <button type="button" onClick={doSave} disabled={busy !== null}>
          {busy === 'save' ? 'Saving…' : 'Save settings'}
        </button>
        <button type="button" className="set-ghost" onClick={doTest} disabled={busy !== null}>
          {busy === 'test' ? 'Testing…' : 'Test connection'}
        </button>
        <button type="button" className="set-ghost" onClick={doScore} disabled={busy !== null}>
          {busy === 'score' ? 'Starting…' : 'Score 10 calls'}
        </button>
      </div>

      {saved && <p className="set-ok">{saved}</p>}
      {error && <p className="set-error">{error}</p>}
      {test && (
        <p className={test.ok ? 'set-ok' : 'set-error'}>
          {test.ok ? '✓ ' : '✗ '}
          {test.message}
        </p>
      )}

      <ScopePanel />
      <ResetPanel />
    </div>
  )
}

export default SettingsScreen
