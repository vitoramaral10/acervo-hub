import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { ArrowLeft, ChevronRight, CircleAlert, Globe, Lock, Plug, Search, SearchX } from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { toast } from 'sonner'
import { SettingField } from '@/components/SettingField'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Badge, Skeleton, Switch } from '@/components/ui/misc'
import { type Definition, api } from '@/lib/api'
import { formatCount } from '@/lib/format'
import { cn } from '@/lib/utils'

export function AddIndexerDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  const [chosen, setChosen] = useState<Definition | null>(null)
  useEffect(() => {
    if (!open) setChosen(null)
  }, [open])
  return (
    <Dialog open={open} onOpenChange={(next) => !next && onClose()}>
      <DialogContent className="max-w-2xl">
        {chosen ? (
          <SettingsStep definition={chosen} onBack={() => setChosen(null)} onDone={onClose} />
        ) : (
          <CatalogStep onChoose={setChosen} />
        )}
      </DialogContent>
    </Dialog>
  )
}

function CatalogStep({ onChoose }: { onChoose: (definition: Definition) => void }) {
  const catalog = useQuery({ queryKey: ['catalogo'], queryFn: api.catalog, staleTime: 5 * 60_000 })
  const [term, setTerm] = useState('')
  const [showUnsupported, setShowUnsupported] = useState(false)

  const all = catalog.data?.definicoes ?? []
  const supportedCount = all.filter((definition) => definition.supported).length
  const list = useMemo(() => {
    const needle = term.trim().toLowerCase()
    return all.filter(
      (definition) =>
        (showUnsupported || definition.supported) &&
        (!needle ||
          definition.name.toLowerCase().includes(needle) ||
          definition.id.includes(needle) ||
          definition.description.toLowerCase().includes(needle)),
    )
  }, [all, term, showUnsupported])

  return (
    <div className="grid gap-5">
      <DialogHeader>
        <DialogTitle>Adicionar indexador</DialogTitle>
        <DialogDescription>
          {catalog.data
            ? `${formatCount(supportedCount)} de ${formatCount(all.length)} definições rodam hoje no acervo-hub.`
            : 'Definições Cardigann do catálogo, mais o Torznab genérico.'}
        </DialogDescription>
      </DialogHeader>

      <div className="grid gap-3 sm:grid-cols-[1fr_auto] sm:items-center">
        <div className="relative">
          <Search
            className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-content-subtle"
            aria-hidden="true"
          />
          <Input
            aria-label="Buscar definição"
            placeholder="Nome do tracker…"
            autoFocus
            value={term}
            onChange={(event) => setTerm(event.target.value)}
            className="pl-9"
          />
        </div>
        <div className="flex items-center gap-2">
          <Switch id="mostrar-nao-suportados" checked={showUnsupported} onCheckedChange={setShowUnsupported} />
          <Label htmlFor="mostrar-nao-suportados" className="font-normal text-content-muted">
            Mostrar os que ainda não rodam
          </Label>
        </div>
      </div>

      <div className="-mx-2 max-h-[min(60dvh,440px)] overflow-y-auto px-2" aria-live="polite">
        {catalog.isPending ? (
          <div className="grid gap-2" aria-busy="true" aria-label="Carregando catálogo">
            {Array.from({ length: 6 }, (_, index) => (
              <Skeleton key={index} className="h-[60px]" />
            ))}
          </div>
        ) : catalog.isError ? (
          <p role="alert" className="rounded-md bg-danger-bg px-3 py-2 text-sm text-danger">
            Não foi possível carregar o catálogo: {catalog.error.message}
          </p>
        ) : list.length === 0 ? (
          <div className="flex flex-col items-center gap-2 py-10 text-center">
            <SearchX className="size-7 text-content-subtle" aria-hidden="true" />
            <p className="text-sm font-medium">Nenhuma definição encontrada</p>
            <p className="text-sm text-content-muted">
              {showUnsupported ? 'Tente outro nome.' : 'Tente outro nome, ou mostre também as que ainda não rodam.'}
            </p>
          </div>
        ) : (
          <ul className="grid gap-1.5">
            {list.map((definition) => (
              <li key={definition.id}>
                <DefinitionRow definition={definition} onChoose={() => onChoose(definition)} />
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  )
}

function DefinitionRow({ definition, onChoose }: { definition: Definition; onChoose: () => void }) {
  const selectable = definition.supported && !definition.added
  const generic = definition.id === 'torznab'
  return (
    <button
      type="button"
      disabled={!selectable}
      onClick={onChoose}
      className={cn(
        'group flex w-full items-center gap-3 rounded-md border border-border px-3 py-2.5 text-left transition-colors',
        selectable
          ? 'hover:border-border-strong hover:bg-surface-raised focus-visible:outline-2 focus-visible:outline-ring'
          : 'cursor-not-allowed opacity-70',
      )}
    >
      <span className="grid size-8 shrink-0 place-items-center rounded-md bg-surface-raised text-content-muted">
        {generic ? <Plug className="size-4" aria-hidden="true" /> : definition.private ? <Lock className="size-4" aria-hidden="true" /> : <Globe className="size-4" aria-hidden="true" />}
      </span>
      <span className="min-w-0 flex-1">
        <span className="flex flex-wrap items-center gap-1.5">
          <span className="font-medium">{definition.name}</span>
          {definition.language && <span className="text-xs text-content-subtle">{definition.language}</span>}
          {definition.private && !generic && <span className="text-xs text-content-subtle">privado</span>}
        </span>
        {definition.supported ? (
          <span className="mt-0.5 line-clamp-1 block text-xs text-content-muted">{definition.description}</span>
        ) : (
          <span className="mt-0.5 line-clamp-2 block text-xs text-warning" title={definition.reason ?? undefined}>
            {definition.reason}
          </span>
        )}
      </span>
      {definition.added ? (
        <Badge>Adicionado</Badge>
      ) : !definition.supported ? (
        <Badge tone="warning">Não suportado</Badge>
      ) : (
        <ChevronRight className="size-4 shrink-0 text-content-subtle transition-transform group-hover:translate-x-0.5" aria-hidden="true" />
      )}
    </button>
  )
}

function SettingsStep({
  definition,
  onBack,
  onDone,
}: {
  definition: Definition
  onBack: () => void
  onDone: () => void
}) {
  const queryClient = useQueryClient()
  const settings = useQuery({
    queryKey: ['definicao-settings', definition.id],
    queryFn: () => api.definitionSettings(definition.id),
  })
  const [values, setValues] = useState<Record<string, string>>({})
  const [failure, setFailure] = useState<string | null>(null)

  useEffect(() => {
    if (!settings.data) return
    setValues(
      Object.fromEntries(
        settings.data.settings.map((setting) => [setting.name, setting.secret ? '' : (setting.value ?? '')]),
      ),
    )
  }, [settings.data])

  const add = useMutation({
    mutationFn: () => api.addIndexer(definition.id, values),
    onSuccess: ({ nome, teste }) => {
      void queryClient.invalidateQueries({ queryKey: ['indexadores'] })
      void queryClient.invalidateQueries({ queryKey: ['catalogo'] })
      if (teste.ok) toast.success(`${nome} adicionado — ${formatCount(teste.resultados)} resultados recentes`)
      else toast.warning(`${nome} adicionado, mas o teste falhou: ${teste.erro ?? 'motivo desconhecido'}`)
      onDone()
    },
    onError: (error: Error) => setFailure(error.message),
  })

  return (
    <form
      noValidate
      className="grid gap-5"
      onSubmit={(event) => {
        event.preventDefault()
        setFailure(null)
        add.mutate()
      }}
    >
      <DialogHeader>
        <button
          type="button"
          onClick={onBack}
          className="mb-1 inline-flex w-fit items-center gap-1 rounded-sm text-sm text-content-muted hover:text-content focus-visible:outline-2 focus-visible:outline-ring"
        >
          <ArrowLeft className="size-4" aria-hidden="true" /> Catálogo
        </button>
        <DialogTitle>{definition.name}</DialogTitle>
        {definition.description && <DialogDescription>{definition.description}</DialogDescription>}
      </DialogHeader>

      {settings.isPending ? (
        <div className="grid gap-4" aria-busy="true" aria-label="Carregando settings">
          {[0, 1].map((key) => (
            <div key={key} className="grid gap-2">
              <Skeleton className="h-3.5 w-24" />
              <Skeleton className="h-9" />
            </div>
          ))}
        </div>
      ) : settings.isError ? (
        <p role="alert" className="rounded-md bg-danger-bg px-3 py-2 text-sm text-danger">
          {settings.error.message}
        </p>
      ) : settings.data.settings.length === 0 ? (
        <p className="text-sm text-content-muted">Esta definição não pede nenhuma configuração.</p>
      ) : (
        <div className="grid gap-4">
          {settings.data.settings.map((setting) => (
            <SettingField
              key={setting.name}
              setting={setting}
              value={values[setting.name] ?? ''}
              onChange={(name, value) => setValues((current) => ({ ...current, [name]: value }))}
            />
          ))}
        </div>
      )}

      {failure && (
        <p role="alert" className="flex gap-2 rounded-md bg-danger-bg px-3 py-2 text-sm text-danger">
          <CircleAlert className="mt-0.5 size-4 shrink-0" aria-hidden="true" />
          <span>{failure}</span>
        </p>
      )}

      <DialogFooter>
        <Button variant="ghost" onClick={onBack}>
          Voltar
        </Button>
        <Button type="submit" variant="primary" loading={add.isPending} disabled={!settings.data}>
          {add.isPending ? 'Adicionando e testando…' : 'Adicionar e testar'}
        </Button>
      </DialogFooter>
    </form>
  )
}
