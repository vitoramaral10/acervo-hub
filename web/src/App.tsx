import { useQuery, useQueryClient } from '@tanstack/react-query'
import { Library, LogOut, Monitor, Moon, Search, Server, Sun } from 'lucide-react'
import { useEffect, useState } from 'react'
import { Button } from '@/components/ui/button'
import { Tooltip } from '@/components/ui/misc'
import { api, isUnauthorized } from '@/lib/api'
import { type Theme, saveTheme, storedTheme } from '@/lib/theme'
import { cn } from '@/lib/utils'
import { IndexersPage } from '@/pages/Indexers'
import { LoginPage } from '@/pages/Login'
import { SearchPage } from '@/pages/Search'

type View = 'indexadores' | 'busca'

function viewFromHash(): View {
  return window.location.hash === '#busca' ? 'busca' : 'indexadores'
}

const NAV: { view: View; label: string; icon: typeof Server }[] = [
  { view: 'indexadores', label: 'Indexadores', icon: Server },
  { view: 'busca', label: 'Busca', icon: Search },
]

export function App() {
  const queryClient = useQueryClient()
  const session = useQuery({ queryKey: ['sessao'], queryFn: api.session, retry: false })
  const [view, setView] = useState<View>(viewFromHash)

  useEffect(() => {
    const onHash = () => setView(viewFromHash())
    window.addEventListener('hashchange', onHash)
    return () => window.removeEventListener('hashchange', onHash)
  }, [])

  // Sessão que expira no meio do uso devolve 401 em qualquer consulta.
  useEffect(
    () =>
      queryClient.getQueryCache().subscribe((event) => {
        if (event.type === 'updated' && isUnauthorized(event.query.state.error)) {
          queryClient.setQueryData(['sessao'], null)
          void queryClient.invalidateQueries({ queryKey: ['sessao'] })
        }
      }),
    [queryClient],
  )

  if (session.isPending) {
    return (
      <div className="grid min-h-dvh place-items-center" aria-busy="true">
        <Library className="size-6 animate-shimmer text-content-subtle" aria-label="Carregando" />
      </div>
    )
  }
  if (session.isError || !session.data) {
    return <LoginPage onLoggedIn={() => void queryClient.invalidateQueries({ queryKey: ['sessao'] })} />
  }

  const logout = async () => {
    await api.logout().catch(() => undefined)
    queryClient.clear()
    await queryClient.invalidateQueries({ queryKey: ['sessao'] })
  }

  return (
    <div className="min-h-dvh md:grid md:grid-cols-[232px_1fr]">
      <a
        href="#conteudo"
        className="sr-only focus:not-sr-only focus:fixed focus:top-3 focus:left-3 focus:z-50 focus:rounded-md focus:bg-surface focus:px-3 focus:py-2"
      >
        Pular para o conteúdo
      </a>
      <aside className="sticky top-0 z-30 flex items-center gap-1 border-b border-border bg-surface px-3 py-2.5 md:h-dvh md:flex-col md:items-stretch md:gap-1 md:border-r md:border-b-0 md:px-3 md:py-5">
        <a href="#indexadores" className="flex shrink-0 items-center gap-2.5 rounded-md p-1 md:mb-6 md:px-2">
          <img src="/ui/icone.svg" alt="" className="size-7" />
          <span className="sr-only leading-tight md:not-sr-only">
            <span className="block text-sm font-semibold tracking-tight">acervo-hub</span>
            <span className="block text-xs text-content-subtle">indexadores</span>
          </span>
        </a>
        <nav aria-label="Seções" className="flex min-w-0 flex-1 gap-1 md:flex-none md:flex-col">
          {NAV.map(({ view: target, label, icon: Icon }) => (
            <a
              key={target}
              href={`#${target}`}
              aria-current={view === target ? 'page' : undefined}
              className={cn(
                'flex items-center gap-2.5 rounded-md px-3 py-2 text-sm font-medium text-content-muted transition-colors hover:bg-surface-raised hover:text-content',
                view === target && 'bg-accent-soft text-accent-soft-fg hover:bg-accent-soft hover:text-accent-soft-fg',
              )}
            >
              <Icon className="size-4" aria-hidden="true" />
              <span>{label}</span>
            </a>
          ))}
        </nav>
        <div className="flex shrink-0 items-center gap-1 md:mt-auto md:flex-col md:items-stretch">
          <div className="hidden md:block">
            <ThemeSwitcher />
          </div>
          <Button variant="ghost" size="sm" onClick={logout} className="md:justify-start">
            <LogOut aria-hidden="true" />
            <span className="hidden md:inline">Sair</span>
            <span className="sr-only md:hidden">Sair</span>
          </Button>
        </div>
      </aside>
      <main id="conteudo" tabIndex={-1} className="min-w-0 px-4 py-6 outline-none sm:px-6 md:px-10 md:py-10">
        <div className="mx-auto max-w-6xl">{view === 'busca' ? <SearchPage /> : <IndexersPage />}</div>
      </main>
    </div>
  )
}

const THEMES: { value: Theme; label: string; icon: typeof Sun }[] = [
  { value: 'light', label: 'Tema claro', icon: Sun },
  { value: 'dark', label: 'Tema escuro', icon: Moon },
  { value: 'system', label: 'Tema do sistema', icon: Monitor },
]

function ThemeSwitcher() {
  const [theme, setTheme] = useState<Theme>(storedTheme)
  return (
    <div role="radiogroup" aria-label="Tema" className="flex rounded-md border border-border p-0.5 md:mb-1 md:self-start">
      {THEMES.map(({ value, label, icon: Icon }) => (
        <Tooltip key={value} content={label}>
          <button
            type="button"
            role="radio"
            aria-checked={theme === value}
            aria-label={label}
            onClick={() => {
              saveTheme(value)
              setTheme(value)
            }}
            className={cn(
              'grid size-7 place-items-center rounded-sm text-content-subtle transition-colors hover:text-content focus-visible:outline-2 focus-visible:outline-ring',
              theme === value && 'bg-surface-raised text-content',
            )}
          >
            <Icon className="size-3.5" aria-hidden="true" />
          </button>
        </Tooltip>
      ))}
    </div>
  )
}
