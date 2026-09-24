import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { Toaster } from 'sonner'
import { App } from '@/App'
import { TooltipProvider } from '@/components/ui/misc'
import { isUnauthorized } from '@/lib/api'
import { applyTheme, storedTheme } from '@/lib/theme'
import './index.css'

// Antes do primeiro render, para não piscar o tema errado.
applyTheme(storedTheme())

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      retry: (count, error) => !isUnauthorized(error) && count < 1,
      refetchOnWindowFocus: false,
    },
  },
})

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <TooltipProvider>
        <App />
        <Toaster position="bottom-right" richColors closeButton theme="system" />
      </TooltipProvider>
    </QueryClientProvider>
  </StrictMode>,
)
