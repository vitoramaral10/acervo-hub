import { useQuery } from '@tanstack/react-query'
import { Check, Search } from 'lucide-react'
import { type FormEvent, useState } from 'react'
import { AddSeriesChoose } from '@/components/AddSeriesChoose'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Badge, Skeleton } from '@/components/ui/misc'
import { type SeriesTmdbResult, seriesApi } from '@/lib/api'

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
          <AddSeriesChoose
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
