import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import './index.css'
import App from './App.tsx'
import { installErrorReporter } from './lib/errorReporter'
import { installAnalytics } from './lib/analytics'

// Report uncaught browser errors to #errors-web before anything mounts.
installErrorReporter()
// GA4 — a no-op unless the build was given VITE_GA_MEASUREMENT_ID.
installAnalytics()

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <App />
  </StrictMode>,
)
