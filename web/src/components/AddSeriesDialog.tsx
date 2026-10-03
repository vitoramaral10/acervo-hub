import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { ArrowLeft, Check, Search, Tv } from 'lucide-react'
import { type FormEvent, useState } from 'react'
import { toast } from 'sonner'
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
import { Badge, Checkbox, Skeleton } from '@/components/ui/misc'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { type SeriesTmdbResult, type WhatToSearch, seriesApi } from '@/lib/api'

const WHAT: { value: WhatToSearch; label: string }[] = [
  { value: 'tudo', label: 'Tudo que falta' },
  { value: 'ultima_temporada', label: 'A partir da última temporada' },
  { value: 'proximos', label: 'Só os próximos' },
]

function Choose({
  result,
  onBack,
  onDone,
}: {
  result: SeriesTmdbResult
  onBack: () => void
  onDone: (id: number) => void
}) {
  const queryClient = useQueryClient()
  const [what, setWhat] = useState<WhatToSearch>('tudo')
  const [monitorNew, setMonitorNew] = useState(true)
  const [seasonFolder, setSeasonFolder] = useState(true)
  const [searchNow, setSearchNow] = useState(true)

  const add = useMutation({
    mutationFn: () =>
      seriesApi.add({
        tmdb: result.tmdb,
        monitorar_novos: monitorNew,
        pasta_de_temporada: seasonFolder,
        buscar: what,
        buscar_agora: searchNow,
      }),
    onSuccess: ({ id }) => {
      toast.success(`${result.titulo} adicionada${searchNow ? ' — buscando' : ''}`)
      void queryClient.invalidateQueries({ queryKey: ['series'] })
      onDone(id)
    },
  })

  return (
    <>
      <div className="flex gap-4">
        <div className="aspect-[2/3] w-24 shrink-0 overflow-hidden rounded-md border border-border bg-surface-raised">
          {result.poster ? (
            <img src={result.poster} alt="" className="size-full object-cover" />
          ) : (
            <Tv className="m-auto mt-8 size-6 text-content-subtle" aria-hidden="true" />
          )}
        </div>
        <div className="min-w-0">
          <p className="text-lg font-semibold">
            {result.titulo}{' '}
            {result.ano && <span className="font-normal text-content-subtle tabular-nums">({result.ano})</span>}
          </p>
          {result.titulo_original && result.titulo_original !== result.titulo && (
            <p className="text-sm text-content-subtle">{result.titulo_original}</p>
          )}
          <p className="mt-2 line-clamp-4 text-sm text-content-muted">{result.sinopse || 'Sem sinopse.'}</p>
        </div>
      </div>

      <div className="grid gap-4">
        <div className="grid gap-2">
          <Label htmlFor="nova-buscar-o-que">Buscar</Label>
          <Select value={what} onValueChange={(value) => setWhat(value as WhatToSearch)}>
            <SelectTrigger id="nova-buscar-o-que">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {WHAT.map(({ value, label }) => (
                <SelectItem key={value} value={value}>
                  {label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          <p className="text-xs text-content-subtle">O que fica de fora entra como “não quero”.</p>
        </div>
        <div className="flex items-center gap-2.5">
          <Checkbox id="nova-monitorar" checked={monitorNew} onChange={(event) => setMonitorNew(event.target.checked)} />
          <Label htmlFor="nova-monitorar" className="font-normal">
            Episódios novos entram como Quero
          </Label>
        </div>
        <div className="flex items-center gap-2.5">
          <Checkbox
            id="nova-pasta"
            checked={seasonFolder}
            onChange={(event) => setSeasonFolder(event.target.checked)}
          />
          <Label htmlFor="nova-pasta" className="font-normal">
            Pasta por temporada
          </Label>
        </div>
        <div className="flex items-center gap-2.5">
          <Checkbox id="nova-agora" checked={searchNow} onChange={(event) => setSearchNow(event.target.checked)} />
          <Label htmlFor="nova-agora" className="font-normal">
            Buscar agora
          </Label>
        </div>
      </div>
      {add.isError && (
        <p role="alert" className="text-sm text-danger">
          {add.error.message}
        </p>
      )}
      <DialogFooter>
        <Button variant="ghost" onClick={onBack}>
          <ArrowLeft aria-hidden="true" />
          Voltar
        </Button>
        <Button variant="primary" loading={add.isPending} onClick={() => add.mutate()}>
          Adicionar série
        </Button>
      </DialogFooter>
    </>
  )
}

/** Busca no TMDB e adiciona à biblioteca. `onOpenSeries` abre uma série que já está nela. */
export function AddSeriesDialog({
  open,
  onOpenChange,
  onOpenSeries,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  onOpenSeries: (id: number) => void
}) {
  const [term, setTerm] = useState('')
  const [submitted, setSubmitted] = useState('')
  const [chosen, setChosen] = useState<SeriesTmdbResult | null>(null)
  const results = useQuery({
    queryKey: ['tmdb-series', submitted],
    queryFn: () => seriesApi.searchTmdb(submitted),
    enabled: submitted.length > 0,
    staleTime: 5 * 60_000,
  })

  const submit = (event: FormEvent) => {
    event.preventDefault()
    setChosen(null)
    setSubmitted(term.trim())
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        onOpenChange(next)
        if (!next) setChosen(null)
      }}
    >
      <DialogContent className="max-w-2xl">
        <DialogHeader>
          <DialogTitle>Adicionar série</DialogTitle>
          <DialogDescription>Busque pelo título (com o ano, se quiser) ou por tmdb:123.</DialogDescription>
        </DialogHeader>
        {chosen ? (
          <Choose
            result={chosen}
            onBack={() => setChosen(null)}
            onDone={(id) => {
              onOpenChange(false)
              setChosen(null)
              onOpenSeries(id)
            }}
          />
        ) : (
          <>
            <form onSubmit={submit} className="flex gap-2" role="search">
              <Label htmlFor="tmdb-busca-serie" className="sr-only">
                Buscar série
              </Label>
              <Input
                id="tmdb-busca-serie"
                autoFocus
                value={term}
                onChange={(event) => setTerm(event.target.value)}
                placeholder="Severance"
              />
              <Button type="submit" variant="primary" loading={results.isFetching} disabled={!term.trim()}>
                {!results.isFetching && <Search aria-hidden="true" />}
                Buscar
              </Button>
            </form>
            <div className="-mx-2 max-h-[55vh] min-h-32 overflow-y-auto px-2" aria-live="polite">
              {!submitted ? (
                <p className="py-10 text-center text-sm text-content-subtle">Os resultados aparecem aqui.</p>
              ) : results.isPending ? (
                <div className="grid gap-2" aria-busy="true">
                  {[0, 1, 2].map((key) => (
                    <Skeleton key={key} className="h-24" />
                  ))}
                </div>
              ) : results.isError ? (
                <p role="alert" className="text-sm text-danger">
                  {results.error.message}
                </p>
              ) : results.data.resultados.length === 0 ? (
                <p className="py-10 text-center text-sm text-content-muted">
                  Nada encontrado para “{submitted}”. Tente o título original ou sem o ano.
                </p>
              ) : (
                <ul className="grid gap-1">
                  {results.data.resultados.map((result) => (
                    <li key={result.tmdb}>
                      <button
                        type="button"
                        onClick={() =>
                          result.no_catalogo != null ? onOpenSeries(result.no_catalogo) : setChosen(result)
                        }
                        className="flex w-full gap-3 rounded-md p-2 text-left transition-colors hover:bg-surface-raised focus-visible:outline-2 focus-visible:outline-ring"
                      >
                        <div className="aspect-[2/3] w-14 shrink-0 overflow-hidden rounded-sm border border-border bg-surface-raised">
                          {result.poster && (
                            <img src={result.poster} alt="" loading="lazy" className="size-full object-cover" />
                          )}
                        </div>
                        <div className="min-w-0 flex-1">
                          <p className="font-medium">
                            {result.titulo}{' '}
                            {result.ano && (
                              <span className="font-normal text-content-subtle tabular-nums">({result.ano})</span>
                            )}
                          </p>
                          {result.titulo_original && result.titulo_original !== result.titulo && (
                            <p className="text-xs text-content-subtle">{result.titulo_original}</p>
                          )}
                          <p className="mt-1 line-clamp-2 text-xs text-content-muted">{result.sinopse}</p>
                        </div>
                        <div className="shrink-0">
                          {result.no_catalogo != null ? (
                            <Badge tone="success">
                              <Check aria-hidden="true" />
                              Na biblioteca
                            </Badge>
                          ) : null}
                        </div>
                      </button>
                    </li>
                  ))}
                </ul>
              )}
            </div>
          </>
        )}
      </DialogContent>
    </Dialog>
  )
}
