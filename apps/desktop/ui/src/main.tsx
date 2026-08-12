import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
// Theme first: global.css pulls in tokens.css, and every component below
// reads those custom properties. The Vite starter's index.css is deliberately
// not imported — its body styles fight the M3 surface tokens.
import './theme/global.css'
import App from './App.tsx'

/**
 * Install the dev fixture backend, then render.
 *
 * The `import.meta.env.DEV` check has to guard the *import*, not just the call.
 * With the guard inside the module, Rollup could not prove the fixture data was
 * unreachable and shipped a fake transcript and scorecard inside the production
 * bundle — invented call content that would eventually be mistaken for real.
 * A statically-false dynamic import drops the module from the graph entirely.
 */
async function boot() {
  if (import.meta.env.DEV) {
    const { installMockBackend } = await import('./dev/mockBackend')
    installMockBackend()
  }

  createRoot(document.getElementById('root')!).render(
    <StrictMode>
      <App />
    </StrictMode>,
  )
}

void boot()
