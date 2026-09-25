import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { CircleCheck, CircleDashed, ExternalLink, KeyRound } from 'lucide-react'
import { useState } from 'react'
import { toast } from 'sonner'
import { PageHeader } from '@/components/PageHeader'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { ExclusionsSection } from '@/pages/settings/Exclusions'
import { FormatsSection } from '@/pages/settings/Formats'
import { ListsSection } from '@/pages/settings/Lists'
import { MigrationSection } from '@/pages/settings/Migration'
import { NotificationsSection } from '@/pages/settings/Notifications'
import { ProfilesSection } from '@/pages/settings/Profiles'
import { OwnerCard, RulesSection, RulesSkeleton, useRules } from '@/pages/settings/Rules'
import { SizesSection } from '@/pages/settings/Sizes'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Badge, Skeleton, Switch } from '@/components/ui/misc'
import { api } from '@/lib/api'

function GeneralSection() {
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
  const automatic = settings.data?.busca_automatica ?? false

  const toggle = useMutation({
    mutationFn: (on: boolean) => api.saveConfiguration({ busca_automatica: on ? 'true' : 'false' }),
    onSuccess: (data) => {
      queryClient.setQueryData(['configuracoes'], data)
      toast.success(data.busca_automatica ? 'Busca automática ligada' : 'Busca automática desligada')
    },
    onError: (error: Error) => toast.error(error.message),
  })

  return (
    <>
      <section aria-labelledby="tmdb-titulo" className="max-w-2xl rounded-lg border border-border bg-surface p-6">
        <div className="flex flex-wrap items-start justify-between gap-3">
          <div>
            <h2 id="tmdb-titulo" className="text-lg font-semibold">
              The Movie Database
            </h2>
            <p className="mt-1 max-w-[60ch] text-sm text-content-muted">
              Fonte dos metadados de filmes: título, ano, datas de lançamento e títulos alternativos. Sem ela, o
              acervo-hub não adiciona filmes nem sabe quando um filme fica disponível.
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

      <section
        aria-labelledby="automatica-titulo"
        className="mt-6 max-w-2xl rounded-lg border border-border bg-surface p-6"
      >
        <div className="flex items-start justify-between gap-6">
          <div>
            <h2 id="automatica-titulo" className="text-lg font-semibold">
              Busca automática
            </h2>
            <p id="automatica-ajuda" className="mt-1 max-w-[60ch] text-sm text-content-muted">
              O acervo-hub lê os releases recentes a cada meia hora e busca os filmes que faltam, e pega sozinho o
              melhor de cada um — upgrades incluídos —, com as regras do Radarr. Ligue só no corte: com o Radarr também
              pegando, cada filme seria baixado duas vezes.
            </p>
          </div>
          {settings.isPending ? (
            <Skeleton className="h-5 w-9 shrink-0" />
          ) : (
            <Switch
              checked={automatic}
              disabled={toggle.isPending || settings.isError}
              onCheckedChange={(on) => toggle.mutate(on)}
              aria-labelledby="automatica-titulo"
              aria-describedby="automatica-ajuda"
              className="mt-1"
            />
          )}
        </div>
      </section>
    </>
  )
}

const TABS = [
  { value: 'geral', label: 'Geral' },
  { value: 'regras', label: 'Regras' },
  { value: 'perfis', label: 'Perfis' },
  { value: 'formatos', label: 'Formatos' },
  { value: 'tamanhos', label: 'Tamanhos' },
  { value: 'notificacoes', label: 'Notificações' },
  { value: 'listas', label: 'Listas' },
  { value: 'exclusoes', label: 'Exclusões' },
  { value: 'migracao', label: 'Migração' },
] as const

type Tab = (typeof TABS)[number]['value']

const TAB_KEY = 'acervo.configuracoes.aba'

function storedTab(): Tab {
  try {
    const value = localStorage.getItem(TAB_KEY)
    return TABS.some((t) => t.value === value) ? (value as Tab) : 'geral'
  } catch {
    return 'geral'
  }
}

export function SettingsPage() {
  const rules = useRules()
  const [tab, setTab] = useState<Tab>(storedTab)
  const editable = rules.data?.dono === 'acervo'
  const hasRadarr = rules.data?.tem_radarr ?? false
  const tabs = TABS.filter((t) => t.value !== 'migracao' || hasRadarr)
  return (
    <>
      <PageHeader
        title="Configurações"
        description="O que o acervo-hub guarda no banco e se muda por aqui, sem reiniciar. Endereços e chaves do cliente de download e dos gerenciadores continuam no config.toml."
      />
      <div className="mb-6">
        {rules.isPending ? (
          <Skeleton className="h-28 rounded-lg" />
        ) : rules.isError ? (
          <div role="alert" className="flex flex-col items-start gap-3 rounded-lg border border-border bg-surface p-6">
            <p className="text-sm text-danger">{rules.error.message}</p>
            <Button onClick={() => void rules.refetch()}>Tentar novamente</Button>
          </div>
        ) : (
          <OwnerCard view={rules.data} />
        )}
      </div>
      <Tabs
        value={tab}
        onValueChange={(value) => {
          setTab(value as Tab)
          try {
            localStorage.setItem(TAB_KEY, value)
          } catch {
            // Sem armazenamento, a aba só não é lembrada.
          }
        }}
      >
        <TabsList aria-label="Seções das configurações">
          {tabs.map((t) => (
            <TabsTrigger key={t.value} value={t.value}>
              {t.label}
            </TabsTrigger>
          ))}
        </TabsList>
        <TabsContent value="geral">
          <GeneralSection />
        </TabsContent>
        <TabsContent value="regras">{rules.data ? <RulesSection view={rules.data} /> : <RulesSkeleton />}</TabsContent>
        <TabsContent value="perfis">
          <ProfilesSection editable={editable} />
        </TabsContent>
        <TabsContent value="formatos">
          <FormatsSection editable={editable} />
        </TabsContent>
        <TabsContent value="tamanhos">
          <SizesSection editable={editable} />
        </TabsContent>
        <TabsContent value="notificacoes">
          <NotificationsSection />
        </TabsContent>
        <TabsContent value="listas">
          <ListsSection />
        </TabsContent>
        <TabsContent value="exclusoes">
          <ExclusionsSection />
        </TabsContent>
        <TabsContent value="migracao">
          <MigrationSection hasRadarr={hasRadarr} />
        </TabsContent>
      </Tabs>
    </>
  )
}
