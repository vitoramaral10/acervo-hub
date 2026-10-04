import { useQuery, useQueryClient } from '@tanstack/react-query'
import {
  Activity,
  Bell,
  CalendarDays,
  ChevronDown,
  CircleDashed,
  Clapperboard,
  Download,
  Eraser,
  Film,
  FolderTree,
  Library,
  ListChecks,
  LogOut,
  Monitor,
  MonitorPlay,
  Moon,
  Scale,
  Search,
  Server,
  ServerCog,
  SlidersHorizontal,
  Sun,
  Tv,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { Button } from '@/components/ui/button'
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { Tooltip } from '@/components/ui/misc'
import { api, isUnauthorized } from '@/lib/api'
import { type Theme, saveTheme, storedTheme } from '@/lib/theme'
import { cn } from '@/lib/utils'
import { ActivityPage } from '@/pages/Activity'
import { CalendarPage } from '@/pages/Calendar'
import { IndexersPage } from '@/pages/Indexers'
import { LoginPage } from '@/pages/Login'
import { MissingPage } from '@/pages/Missing'
import { MoviesPage } from '@/pages/Movies'
import { SearchPage } from '@/pages/Search'
import { SeriesPage } from '@/pages/Series'
import { SettingsPage, type SettingsView } from '@/pages/Settings'
import { TasksPage } from '@/pages/Tasks'

type MainView = 'filmes' | 'series' | 'faltando' | 'calendario' | 'atividade' | 'busca'
type ConfigView = 'indexadores' | 'tarefas' | SettingsView
type View = MainView | ConfigView

type NavItem<V extends View> = { view: V; label: string; icon: typeof Server }

/** Uso do dia a dia: sempre à vista. */
const MAIN: NavItem<MainView>[] = [
  { view: 'filmes', label: 'Filmes', icon: Film },
  { view: 'series', label: 'Séries', icon: MonitorPlay },
  { view: 'faltando', label: 'Faltando', icon: CircleDashed },
  { view: 'calendario', label: 'Calendário', icon: CalendarDays },
  { view: 'atividade', label: 'Atividade', icon: Activity },
  { view: 'busca', label: 'Busca', icon: Search },
]

/** Configuração do serviço: num grupo que se recolhe. */
const CONFIG: NavItem<ConfigView>[] = [
  { view: 'indexadores', label: 'Indexadores', icon: Server },
  { view: 'tarefas', label: 'Tarefas', icon: ListChecks },
  { view: 'cliente-download', label: 'Cliente de download', icon: Download },
  { view: 'jellyfin', label: 'Jellyfin', icon: Tv },
  { view: 'biblioteca', label: 'Biblioteca', icon: FolderTree },
  { view: 'limpeza', label: 'Limpeza', icon: Eraser },
  { view: 'regras', label: 'Regras de decisão', icon: Scale },
  { view: 'notificacoes', label: 'Notificações', icon: Bell },
  { view: 'tmdb', label: 'TMDB', icon: Clapperboard },
  { view: 'servidor', label: 'Servidor', icon: ServerCog },
]

const VIEWS: View[] = [...MAIN, ...CONFIG].map((item) => item.view)
const inConfig = (view: View) => CONFIG.some((item) => item.view === view)

function viewFromHash(): View {
  // A query depois de `?` é da tela (ex.: `#filmes?ordem=ano`), não faz parte do nome.
  const hash = window.location.hash.slice(1).split('?')[0]
  // Endereço salvo da antiga página única de configurações.
  if (hash === 'configuracoes') return 'servidor'
  return VIEWS.includes(hash as View) ? (hash as View) : 'filmes'
}

const GROUP_KEY = 'acervo.menu.configuracoes'

function storedGroup(): boolean {
  try {
    return localStorage.getItem(GROUP_KEY) === 'aberto'
  } catch {
    return false
  }
}

function NavLink({ item, current, onNavigate }: { item: NavItem<View>; current: View; onNavigate?: () => void }) {
  const Icon = item.icon
  const active = current === item.view
  return (
    <a
      href={`#${item.view}`}
      onClick={onNavigate}
      aria-current={active ? 'page' : undefined}
      className={cn(
        'flex items-center gap-2.5 rounded-md px-3 py-2 text-sm font-medium text-content-muted transition-colors hover:bg-surface-raised hover:text-content focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-ring',
        active && 'bg-accent-soft text-accent-soft-fg hover:bg-accent-soft hover:text-accent-soft-fg',
      )}
    >
      <Icon className="size-4 shrink-0" aria-hidden="true" />
      <span>{item.label}</span>
    </a>
  )
}

/** No desktop: acordeão fechado por padrão, aberto sozinho quando a tela atual está dentro dele. */
function ConfigGroup({ current }: { current: View }) {
  const [open, setOpen] = useState(() => storedGroup() || inConfig(current))
  useEffect(() => {
    if (inConfig(current)) setOpen(true)
  }, [current])
  const toggle = () => {
    const next = !open
    setOpen(next)
    try {
      localStorage.setItem(GROUP_KEY, next ? 'aberto' : 'fechado')
    } catch {
      // Sem armazenamento, o grupo só não é lembrado.
    }
  }
  return (
    <div className="mt-4 border-t border-border pt-4">
      <button
        type="button"
        onClick={toggle}
        aria-expanded={open}
        aria-controls="menu-configuracoes"
        className="flex w-full items-center gap-2.5 rounded-md px-3 py-2 text-left text-sm font-medium text-content-muted transition-colors hover:bg-surface-raised hover:text-content focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-ring"
      >
        <SlidersHorizontal className="size-4 shrink-0" aria-hidden="true" />
        <span className="flex-1">Configurações</span>
        <ChevronDown className={cn('size-4 shrink-0 transition-transform', open && 'rotate-180')} aria-hidden="true" />
      </button>
      <div id="menu-configuracoes" hidden={!open} className="mt-1 grid gap-0.5 pl-2">
        {CONFIG.map((item) => (
          <NavLink key={item.view} item={item} current={current} />
        ))}
      </div>
    </div>
  )
}

/** No celular: um botão que abre a folha com os itens do grupo. */
function ConfigSheet({ current }: { current: View }) {
  const [open, setOpen] = useState(false)
  const active = inConfig(current)
  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <button
        type="button"
        onClick={() => setOpen(true)}
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-controls="folha-configuracoes"
        className={cn(
          'flex items-center gap-2.5 rounded-md px-3 py-2 text-sm font-medium text-content-muted transition-colors hover:bg-surface-raised hover:text-content focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-ring',
          active && 'bg-accent-soft text-accent-soft-fg',
        )}
      >
        <SlidersHorizontal className="size-4" aria-hidden="true" />
        <span className="sr-only sm:not-sr-only">Configurações</span>
      </button>
      <DialogContent id="folha-configuracoes" className="max-w-sm">
        <DialogHeader>
          <DialogTitle>Configurações</DialogTitle>
          <DialogDescription>Indexadores, tarefas e cada seção do serviço.</DialogDescription>
        </DialogHeader>
        <nav aria-label="Configurações" className="grid gap-0.5">
          {CONFIG.map((item) => (
            <NavLink key={item.view} item={item} current={current} onNavigate={() => setOpen(false)} />
          ))}
        </nav>
      </DialogContent>
    </Dialog>
  )
}

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
        <a href="#filmes" className="flex shrink-0 items-center gap-2.5 rounded-md p-1 md:mb-6 md:px-2">
          <img src="/ui/icone.svg" alt="" className="size-7" />
          <span className="sr-only leading-tight md:not-sr-only">
            <span className="block text-sm font-semibold tracking-tight">acervo-hub</span>
            <span className="block text-xs text-content-subtle">mídia</span>
          </span>
        </a>
        {/* Celular: barra horizontal, com o grupo de configurações numa folha. */}
        <nav aria-label="Seções" className="flex min-w-0 flex-1 gap-1 overflow-x-auto md:hidden">
          {MAIN.map(({ view: target, label, icon: Icon }) => (
            <a
              key={target}
              href={`#${target}`}
              aria-current={view === target ? 'page' : undefined}
              aria-label={label}
              className={cn(
                'flex items-center gap-2.5 rounded-md px-3 py-2 text-sm font-medium text-content-muted transition-colors hover:bg-surface-raised hover:text-content focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-ring',
                view === target && 'bg-accent-soft text-accent-soft-fg hover:bg-accent-soft hover:text-accent-soft-fg',
              )}
            >
              <Icon className="size-4" aria-hidden="true" />
              <span className="sr-only sm:not-sr-only">{label}</span>
            </a>
          ))}
          <ConfigSheet current={view} />
        </nav>
        {/* Desktop: coluna, com o grupo de configurações em acordeão. */}
        <nav aria-label="Seções" className="hidden min-h-0 flex-col gap-1 overflow-y-auto md:flex">
          {MAIN.map((item) => (
            <NavLink key={item.view} item={item} current={view} />
          ))}
          <ConfigGroup current={view} />
        </nav>
        <div className="flex shrink-0 items-center gap-1 md:mt-auto md:flex-col md:items-stretch">
          <div className="hidden md:block">
            <ThemeSwitcher />
          </div>
          {session.data.usuario && (
            <p className="hidden truncate px-3 pt-2 text-xs text-content-subtle md:block">
              Conectado como <span className="font-medium text-content-muted">{session.data.usuario}</span>
            </p>
          )}
          <Button variant="ghost" size="sm" onClick={logout} className="md:justify-start">
            <LogOut aria-hidden="true" />
            <span className="hidden md:inline">Sair</span>
            <span className="sr-only md:hidden">Sair</span>
          </Button>
        </div>
      </aside>
      <main id="conteudo" tabIndex={-1} className="min-w-0 px-4 py-6 outline-none sm:px-6 md:px-10 md:py-10">
        <div className="mx-auto max-w-6xl">
          {view === 'busca' ? (
            <SearchPage />
          ) : view === 'filmes' ? (
            <MoviesPage />
          ) : view === 'series' ? (
            <SeriesPage />
          ) : view === 'faltando' ? (
            <MissingPage />
          ) : view === 'calendario' ? (
            <CalendarPage />
          ) : view === 'atividade' ? (
            <ActivityPage />
          ) : view === 'tarefas' ? (
            <TasksPage />
          ) : view === 'indexadores' ? (
            <IndexersPage />
          ) : (
            <SettingsPage view={view} />
          )}
        </div>
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
    <div
      role="radiogroup"
      aria-label="Tema"
      className="flex rounded-md border border-border p-0.5 md:mb-1 md:self-start"
    >
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
