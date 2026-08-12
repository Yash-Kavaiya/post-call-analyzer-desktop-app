/**
 * App shell: navigation rail plus the active screen.
 *
 * Routing is a small piece of state rather than a router library — there are
 * four destinations and one drill-down, and a router would add a dependency
 * for no benefit.
 */

import { useEffect, useState } from 'react'
import { CallDetailScreen } from './screens/CallDetailScreen'
import { CallsScreen } from './screens/CallsScreen'
import { DashboardScreen } from './screens/DashboardScreen'
import { IngestScreen } from './screens/IngestScreen'
import { InsightsScreen } from './screens/InsightsScreen'
import { SettingsScreen } from './screens/SettingsScreen'
import './App.css'

type Tab = 'dashboard' | 'ingest' | 'calls' | 'insights' | 'settings'

const TABS: { id: Tab; label: string; icon: string }[] = [
  { id: 'dashboard', label: 'Overview', icon: '◱' },
  { id: 'ingest', label: 'Ingest', icon: '↧' },
  { id: 'calls', label: 'Calls', icon: '☰' },
  { id: 'insights', label: 'Insights', icon: '◈' },
  { id: 'settings', label: 'Settings', icon: '⚙︎' },
]

type Theme = 'light' | 'dark'

export default function App() {
  const [tab, setTab] = useState<Tab>('dashboard')
  const [selected, setSelected] = useState<string | null>(null)
  const [theme, setTheme] = useState<Theme>(() =>
    window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light',
  )

  useEffect(() => {
    document.documentElement.setAttribute('data-theme', theme)
  }, [theme])

  const openCall = (callId: string) => {
    setSelected(callId)
  }

  return (
    <div className="app-root">
      <nav className="app-rail">
        <div className="app-brand" title="Call Analyzer">
          ◐
        </div>
        {TABS.map((t) => (
          <button
            key={t.id}
            type="button"
            className={`app-rail-item${tab === t.id && !selected ? ' app-active' : ''}`}
            onClick={() => {
              setTab(t.id)
              setSelected(null)
            }}
          >
            <span className="app-rail-icon">{t.icon}</span>
            <span className="app-rail-label">{t.label}</span>
          </button>
        ))}
        <button
          type="button"
          className="app-rail-item app-theme"
          onClick={() => setTheme((t) => (t === 'dark' ? 'light' : 'dark'))}
          title="Toggle theme"
        >
          {/* U+FE0E forces text presentation: without it Chrome renders the sun
              as a colour emoji, which fights the monochrome rail. */}
          <span className="app-rail-icon">{theme === 'dark' ? '☀︎' : '☾︎'}</span>
          <span className="app-rail-label">Theme</span>
        </button>
      </nav>

      <main className="app-main">
        {selected ? (
          <CallDetailScreen callId={selected} onBack={() => setSelected(null)} />
        ) : tab === 'dashboard' ? (
          <DashboardScreen onSelect={openCall} />
        ) : tab === 'ingest' ? (
          <IngestScreen />
        ) : tab === 'insights' ? (
          <InsightsScreen />
        ) : tab === 'settings' ? (
          <SettingsScreen />
        ) : (
          <CallsScreen onSelect={openCall} />
        )}
      </main>
    </div>
  )
}
