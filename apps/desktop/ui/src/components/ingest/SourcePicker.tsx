/**
 * Where calls come from: a folder, one file, or a public bucket.
 *
 * The three cloud services differ in what they call things — S3 has buckets and
 * regions, Azure has containers under a storage account, Google has buckets and
 * nothing else — so the form changes shape rather than showing every field and
 * greying most of them out. The URL that will be contacted is assembled live
 * and shown, because a scan that 404s is almost always a name in the wrong box.
 */

import { useCallback, useMemo, useState } from 'react'
import { open } from '@tauri-apps/plugin-dialog'
import {
  discoverCloud,
  discoverPath,
  type CloudProviderId,
  type CloudSpec,
} from '../../api'
import './SourcePicker.css'

type Mode = 'folder' | 'file' | 'cloud'

const MODES: { id: Mode; label: string; hint: string }[] = [
  { id: 'folder', label: 'Folder', hint: 'Every recording in a folder and its subfolders.' },
  { id: 'file', label: 'Single file', hint: 'One recording.' },
  { id: 'cloud', label: 'Cloud bucket', hint: 'A public bucket on S3, Azure or Google Cloud.' },
]

const CLOUD_LABELS: Record<CloudProviderId, string> = {
  s3: 'Amazon S3 (or S3-compatible)',
  azure: 'Azure Blob Storage',
  gcs: 'Google Cloud Storage',
}

/** Audio containers the decoder handles, for the file picker's filter. */
const AUDIO_EXTS = ['m4a', 'mp3', 'wav', 'flac', 'ogg', 'opus', 'mp4', 'webm', 'aac', 'mka', 'mkv']

const EMPTY: CloudSpec = {
  provider: 's3',
  bucket: '',
  prefix: '',
  region: 'us-east-1',
  account: '',
  endpoint: '',
  includeTranscripts: false,
}

/** Mirrors the Rust `base_url`, so what is shown is what will be contacted. */
function previewUrl(spec: CloudSpec): string {
  const bucket = spec.bucket.trim() || '<bucket>'
  switch (spec.provider) {
    case 's3': {
      const endpoint = (spec.endpoint ?? '').trim()
      if (endpoint) return `${endpoint.replace(/\/+$/, '')}/${bucket}`
      return `https://${bucket}.s3.${(spec.region ?? '').trim() || 'us-east-1'}.amazonaws.com`
    }
    case 'azure':
      return `https://${(spec.account ?? '').trim() || '<account>'}.blob.core.windows.net/${bucket}`
    case 'gcs':
      return `https://storage.googleapis.com/${bucket}`
  }
}

export interface SourcePickerProps {
  busy: boolean
  onBusy: (busy: boolean) => void
  onResult: (message: string) => void
  onError: (message: string) => void
}

export function SourcePicker({ busy, onBusy, onResult, onError }: SourcePickerProps) {
  const [mode, setMode] = useState<Mode>('folder')
  const [path, setPath] = useState<string | null>(null)
  const [spec, setSpec] = useState<CloudSpec>(EMPTY)

  const update = useCallback((patch: Partial<CloudSpec>) => {
    setSpec((prev) => ({ ...prev, ...patch }))
  }, [])

  const pick = useCallback(async () => {
    try {
      const chosen = await open(
        mode === 'folder'
          ? { directory: true, multiple: false }
          : {
              multiple: false,
              filters: [{ name: 'Audio', extensions: AUDIO_EXTS }],
            },
      )
      if (typeof chosen === 'string') setPath(chosen)
    } catch (e) {
      onError(String(e))
    }
  }, [mode, onError])

  const discover = useCallback(async () => {
    onBusy(true)
    try {
      const { total, added } =
        mode === 'cloud' ? await discoverCloud(spec) : await discoverPath(path ?? '')
      onResult(
        total === 0
          ? 'Nothing readable found there.'
          : `Found ${total} recording${total === 1 ? '' : 's'} — ${added} newly registered, ${
              total - added
            } already known.`,
      )
    } catch (e) {
      onError(String(e))
    } finally {
      onBusy(false)
    }
  }, [mode, spec, path, onBusy, onResult, onError])

  const ready = useMemo(() => {
    if (mode === 'cloud') {
      if (!spec.bucket.trim()) return false
      if (spec.provider === 'azure' && !(spec.account ?? '').trim()) return false
      return true
    }
    return path !== null
  }, [mode, spec, path])

  const activeMode = MODES.find((m) => m.id === mode)!

  return (
    <div className="sp-root">
      <div className="sp-modes" role="tablist" aria-label="Source type">
        {MODES.map((m) => (
          <button
            key={m.id}
            type="button"
            role="tab"
            aria-selected={mode === m.id}
            className={`sp-mode${mode === m.id ? ' sp-mode-on' : ''}`}
            onClick={() => {
              setMode(m.id)
              setPath(null)
            }}
          >
            {m.label}
          </button>
        ))}
      </div>
      <p className="sp-hint">{activeMode.hint}</p>

      {mode !== 'cloud' ? (
        <div className="sp-row">
          <button type="button" className="sp-btn" onClick={pick} disabled={busy}>
            {mode === 'folder' ? 'Choose folder…' : 'Choose file…'}
          </button>
          <code className="sp-path" title={path ?? undefined}>
            {path ?? (mode === 'folder' ? 'No folder selected' : 'No file selected')}
          </code>
        </div>
      ) : (
        <>
          <div className="sp-grid">
            <label className="sp-field">
              <span>Service</span>
              <select
                value={spec.provider}
                onChange={(e) => update({ provider: e.target.value as CloudProviderId })}
              >
                {(Object.keys(CLOUD_LABELS) as CloudProviderId[]).map((id) => (
                  <option key={id} value={id}>
                    {CLOUD_LABELS[id]}
                  </option>
                ))}
              </select>
            </label>

            {spec.provider === 'azure' && (
              <label className="sp-field">
                <span>Storage account</span>
                <input
                  value={spec.account ?? ''}
                  onChange={(e) => update({ account: e.target.value })}
                  placeholder="mystorageaccount"
                  spellCheck={false}
                />
              </label>
            )}

            <label className="sp-field">
              <span>{spec.provider === 'azure' ? 'Container' : 'Bucket'}</span>
              <input
                value={spec.bucket}
                onChange={(e) => update({ bucket: e.target.value })}
                placeholder={spec.provider === 'azure' ? 'recordings' : 'my-call-recordings'}
                spellCheck={false}
              />
            </label>

            {spec.provider === 's3' && (
              <label className="sp-field">
                <span>Region</span>
                <input
                  value={spec.region ?? ''}
                  onChange={(e) => update({ region: e.target.value })}
                  placeholder="us-east-1"
                  spellCheck={false}
                />
              </label>
            )}

            <label className="sp-field">
              <span>Prefix (optional)</span>
              <input
                value={spec.prefix}
                onChange={(e) => update({ prefix: e.target.value })}
                placeholder="inbound/2026-08/"
                spellCheck={false}
              />
            </label>

            {spec.provider === 's3' && (
              <label className="sp-field">
                <span>Custom endpoint (optional)</span>
                <input
                  value={spec.endpoint ?? ''}
                  onChange={(e) => update({ endpoint: e.target.value })}
                  placeholder="https://minio.example.com — for MinIO, R2, Wasabi"
                  spellCheck={false}
                />
              </label>
            )}
          </div>

          <p className="sp-preview">
            Reads from <code>{previewUrl(spec)}</code>
          </p>
          <p className="sp-hint">
            Public buckets only — anonymous reads, nothing is signed. Objects are listed now
            and downloaded later, as the batch reaches them.
          </p>
        </>
      )}

      <button type="button" className="sp-btn" onClick={discover} disabled={!ready || busy}>
        {busy ? 'Scanning…' : 'Scan for recordings'}
      </button>
    </div>
  )
}

export default SourcePicker
