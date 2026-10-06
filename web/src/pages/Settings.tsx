import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { CircleCheck, CircleDashed, ExternalLink, KeyRound } from 'lucide-react'
import { type ReactNode, useEffect, useState } from 'react'
import { toast } from 'sonner'
import { PageHeader } from '@/components/PageHeader'
import { NotificationsSection } from '@/pages/settings/Notifications'
import { RulesSection, RulesSkeleton, useRules } from '@/pages/settings/Rules'
import {
  DownloadClientSettings,
  JellyfinSettings,
  LibrarySettings,
  ServerSettings,
} from '@/pages/settings/Sections'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Badge, Skeleton } from '@/components/ui/misc'
import { api } from '@/lib/api'

function TmdbSettings() {
  const queryClient = useQueryClient()
  const settings = useQuery({ queryKey: ['configuracoes'], queryFn: api.configuration })
  const [key, setKey] = useState('')
  const [shownError, setShownError] = useState<string | null>(null)

  const save = useMutation({
    mutationFn: (value: string | null) => api.saveConfiguration({ tmdb_chave: value }),
    onSuccess: (data, value) => {
      queryClient.setQueryData(['configuracoes'], data)
      setKey('')
      setShownError(null)
      toast.success(value === null ? 'Chave do TMDB removida' : 'Chave do TMDB conferida e salva')
    },
    onError: (error: Error) => setShownError(error.message),
  })

  const defined = settings.data?.tmdb.definida ?? false

  return (
    <>
      <section aria-labelledby="tmdb-titulo" className="max-w-2xl rounded-lg border border-border bg-surface p-6">
        <div className="flex flex-wrap items-start justify-between gap-3">
          <div>
            <h2 id="tmdb-titulo" className="text-lg font-semibold">
              The Movie Database
            </h2>
            <p className="mt-1 max-w-[60ch] text-sm text-content-muted">
              Fonte dos metadados de filmes e séries: títulos, sinopses, lançamentos e episódios. Necessária
              para a busca global e para adicionar títulos ao acervo.
            </p>
          </div>
          {settings.isPending ? (
            <Skeleton className="h-6 w-24" />
          ) : (
            <Badge tone={defined ? 'success' : undefined}>
              {defined ? <CircleCheck aria-hidden="true" /> : <CircleDashed aria-hidden="true" />}
              {defined ? 'Configurada' : 'Não configurada'}
            </Badge>
          )}
        </div>

        {settings.isError ? (
          <div role="alert" className="mt-6 flex flex-col items-start gap-3">
            <p className="text-sm text-danger">{settings.error.message}</p>
            <Button onClick={() => void settings.refetch()}>Tentar novamente</Button>
          </div>
        ) : (
          <form
            noValidate
            className="mt-6 grid gap-4"
            onSubmit={(event) => {
              event.preventDefault()
              if (!key.trim()) {
                setShownError('Cole a chave da API (v3) antes de salvar.')
                return
              }
              save.mutate(key.trim())
            }}
          >
            <div className="grid gap-2">
              <Label htmlFor="tmdb-chave">Chave da API (v3)</Label>
              <div className="relative">
                <KeyRound
                  className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-content-subtle"
                  aria-hidden="true"
                />
                <Input
                  id="tmdb-chave"
                  type="password"
                  autoComplete="off"
                  spellCheck={false}
                  value={key}
                  onChange={(event) => setKey(event.target.value)}
                  placeholder={defined ? 'Definida — cole outra para trocar' : ''}
                  aria-invalid={shownError ? true : undefined}
                  aria-describedby="tmdb-ajuda tmdb-erro"
                  className="pl-9 font-mono"
                />
              </div>
              <p id="tmdb-ajuda" className="text-xs text-content-subtle">
                Gratuita: crie uma conta e gere em{' '}
                <a
                  href="https://www.themoviedb.org/settings/api"
                  target="_blank"
                  rel="noreferrer"
                  className="inline-flex items-center gap-0.5 underline underline-offset-2 hover:text-content"
                >
                  themoviedb.org/settings/api
                  <ExternalLink className="size-3" aria-hidden="true" />
                </a>
                . A chave é testada antes de salvar e nunca volta para a tela.
              </p>
              {shownError && (
                <p id="tmdb-erro" role="alert" className="text-sm text-danger">
                  {shownError}
                </p>
              )}
            </div>
            <div className="flex flex-wrap gap-2">
              <Button type="submit" variant="primary" loading={save.isPending && save.variables !== null}>
                Testar e salvar
              </Button>
              {defined && (
                <Button
                  type="button"
                  variant="ghost"
                  loading={save.isPending && save.variables === null}
                  onClick={() => save.mutate(null)}
                >
                  Remover chave
                </Button>
              )}
            </div>
          </form>
        )}
      </section>
    </>
  )
}

/** Seções editáveis reunidas na mesma tela. */
type Section = 'servidor' | 'cliente-download' | 'jellyfin' | 'biblioteca' | 'regras' | 'notificacoes' | 'tmdb'

const PAGES: Record<Section, { title: string; description: string; body: () => ReactNode }> = {
  servidor: {
    title: 'Servidor',
    description: 'A chave de API, a rede dos indexadores e o catálogo de definições. Vale na hora, sem reiniciar.',
    body: () => <ServerSettings />,
  },
  'cliente-download': {
    title: 'Cliente de download',
    description: 'O qBittorrent que recebe os torrents do acervo e por onde a limpeza age.',
    body: () => <DownloadClientSettings />,
  },
  jellyfin: {
    title: 'Jellyfin',
    description: 'O servidor de mídia que diz o que já foi assistido.',
    body: () => <JellyfinSettings />,
  },
  biblioteca: {
    title: 'Biblioteca',
    description: 'Pastas, caminhos e a categoria dos downloads do acervo.',
    body: () => <LibrarySettings />,
  },
  regras: {
    title: 'Regras de decisão',
    description: 'O que vale para todo release na hora de escolher.',
    body: () => <RulesBody />,
  },
  notificacoes: {
    title: 'Notificações',
    description: 'Avisos por push do que o acervo faz.',
    body: () => <NotificationsSection />,
  },
  tmdb: {
    title: 'TMDB',
    description: 'A fonte dos metadados de filmes e séries.',
    body: () => <TmdbSettings />,
  },
}

function RulesBody() {
  const rules = useRules()
  return rules.isError ? (
    <div role="alert" className="flex flex-col items-start gap-3 rounded-lg border border-border bg-surface p-6">
      <p className="text-sm text-danger">{rules.error.message}</p>
      <Button onClick={() => void rules.refetch()}>Tentar novamente</Button>
    </div>
  ) : rules.data ? (
    <RulesSection view={rules.data} />
  ) : (
    <RulesSkeleton />
  )
}

function sectionFromHash(): Section {
  const [view, query] = window.location.hash.slice(1).split('?')
  const name = view === 'configuracoes' ? new URLSearchParams(query).get('secao') : view
  return name && name in PAGES ? name as Section : 'servidor'
}

export function SettingsPage() {
  const [section, setSection] = useState<Section>(sectionFromHash)
  useEffect(() => {
    const change = () => setSection(sectionFromHash())
    window.addEventListener('hashchange', change)
    return () => window.removeEventListener('hashchange', change)
  }, [])
  return (
    <>
      <PageHeader title="Configurações" description="Conexões, biblioteca e regras do serviço." />
      <div className="mb-6 flex flex-wrap gap-2" role="group" aria-label="Seções de configuração">
        {(Object.keys(PAGES) as Section[]).map((name) => (
          <Button
            key={name}
            size="sm"
            variant={section === name ? 'primary' : 'secondary'}
            aria-pressed={section === name}
            onClick={() => { window.location.hash = `configuracoes?secao=${name}` }}
          >
            {PAGES[name].title}
          </Button>
        ))}
      </div>
      {PAGES[section].body()}
    </>
  )
}
