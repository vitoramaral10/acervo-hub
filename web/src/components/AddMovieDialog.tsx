import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { ArrowLeft, Check, Film, Search } from 'lucide-react'
import { type FormEvent, useEffect, useState } from 'react'
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
import { Badge, Skeleton, Switch } from '@/components/ui/misc'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { type TmdbResult, library } from '@/lib/api'
import { formatSize } from '@/lib/format'

export const AVAILABILITIES = [
  { value: 'announced', label: 'Anunciado' },
  { value: 'inCinemas', label: 'No cinema' },
  { value: 'released', label: 'Lançado' },
]

function Choose({ result, onBack, onDone }: { result: TmdbResult; onBack: () => void; onDone: (id: number) => void }) {
  const queryClient = useQueryClient()
  const options = useQuery({ queryKey: ['biblioteca-opcoes'], queryFn: library.options })
  const [profile, setProfile] = useState('')
  const [folder, setFolder] = useState('')
  const [availability, setAvailability] = useState('released')
  const [monitored, setMonitored] = useState(true)
  const [search, setSearch] = useState(true)

  useEffect(() => {
    if (!options.data) return
    setProfile((current) => current || options.data.perfis[0]?.nome || '')
    setFolder((current) => current || options.data.pastas[0]?.caminho || '')
  }, [options.data])

  const add = useMutation({
    mutationFn: () =>
      library.add({
        tmdb: result.tmdb,
        perfil: profile,
        pasta: folder,
        monitorado: monitored,
        disponibilidade_minima: availability,
        tags: [],
        buscar: search && monitored,
      }),
    onSuccess: ({ id }) => {
      toast.success(`${result.titulo} adicionado${search && monitored ? ' — buscando' : ''}`)
      void queryClient.invalidateQueries({ queryKey: ['filmes'] })
      onDone(id)
    },
  })

  const radarr = options.data?.dono_das_regras === 'radarr'

  return (
    <>
      <div className="flex gap-4">
        <div className="aspect-[2/3] w-24 shrink-0 overflow-hidden rounded-md border border-border bg-surface-raised">
          {result.poster ? (
            <img src={result.poster} alt="" className="size-full object-cover" />
          ) : (
            <Film className="m-auto mt-8 size-6 text-content-subtle" aria-hidden="true" />
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

      {options.isPending ? (
        <Skeleton className="h-40" />
      ) : options.isError ? (
        <p role="alert" className="text-sm text-danger">
          {options.error.message}
        </p>
      ) : (
        <div className="grid gap-4 sm:grid-cols-2">
          <div className="grid gap-2">
            <Label htmlFor="novo-perfil">Perfil de qualidade</Label>
            <Select value={profile} onValueChange={setProfile}>
              <SelectTrigger id="novo-perfil">
                <SelectValue placeholder="Escolha" />
              </SelectTrigger>
              <SelectContent>
                {options.data.perfis.map((p) => (
                  <SelectItem key={p.id} value={p.nome}>
                    {p.nome}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
          <div className="grid gap-2">
            <Label htmlFor="novo-disponibilidade">Pegar a partir de</Label>
            <Select value={availability} onValueChange={setAvailability}>
              <SelectTrigger id="novo-disponibilidade">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {AVAILABILITIES.map((a) => (
                  <SelectItem key={a.value} value={a.value}>
                    {a.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
          <div className="grid gap-2 sm:col-span-2">
            <Label htmlFor="novo-pasta">Pasta</Label>
            <Select value={folder} onValueChange={setFolder}>
              <SelectTrigger id="novo-pasta">
                <SelectValue placeholder="Escolha" />
              </SelectTrigger>
              <SelectContent>
                {options.data.pastas.map((p) => (
                  <SelectItem key={p.caminho} value={p.caminho}>
                    {p.caminho}
                    {p.livre != null && ` — ${formatSize(p.livre)} livres`}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
          <div className="flex items-center justify-between gap-4">
            <Label htmlFor="novo-monitorado">Monitorado</Label>
            <Switch id="novo-monitorado" checked={monitored} onCheckedChange={setMonitored} />
          </div>
          <div className="flex items-center justify-between gap-4">
            <Label htmlFor="novo-buscar">Buscar agora</Label>
            <Switch id="novo-buscar" checked={search && monitored} disabled={!monitored} onCheckedChange={setSearch} />
          </div>
          {radarr && (
            <p className="text-xs text-content-subtle sm:col-span-2">
              As regras ainda são do Radarr: o filme é adicionado lá, e ele busca e baixa.
            </p>
          )}
        </div>
      )}
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
        <Button variant="primary" disabled={!profile || !folder} loading={add.isPending} onClick={() => add.mutate()}>
          Adicionar filme
        </Button>
      </DialogFooter>
    </>
  )
}

/** Busca no TMDB e adiciona à biblioteca. `onOpenMovie` abre um filme que já está nela. */
export function AddMovieDialog({
  open,
  onOpenChange,
  onOpenMovie,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  onOpenMovie: (id: number) => void
}) {
  const [term, setTerm] = useState('')
  const [submitted, setSubmitted] = useState('')
  const [chosen, setChosen] = useState<TmdbResult | null>(null)
  const results = useQuery({
    queryKey: ['tmdb', submitted],
    queryFn: () => library.searchTmdb(submitted),
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
        if (!next) {
          setChosen(null)
        }
      }}
    >
      <DialogContent className="max-w-2xl">
        <DialogHeader>
          <DialogTitle>Adicionar filme</DialogTitle>
          <DialogDescription>
            Busque pelo título (com o ano, se quiser), por tmdb:123 ou pelo id do IMDb.
          </DialogDescription>
        </DialogHeader>
        {chosen ? (
          <Choose
            result={chosen}
            onBack={() => setChosen(null)}
            onDone={(id) => {
              onOpenChange(false)
              setChosen(null)
              onOpenMovie(id)
            }}
          />
        ) : (
          <>
            <form onSubmit={submit} className="flex gap-2" role="search">
              <Label htmlFor="tmdb-busca" className="sr-only">
                Buscar filme
              </Label>
              <Input
                id="tmdb-busca"
                autoFocus
                value={term}
                onChange={(event) => setTerm(event.target.value)}
                placeholder="Duna 2021"
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
                          result.no_catalogo != null ? onOpenMovie(result.no_catalogo) : setChosen(result)
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
                          ) : result.excluido ? (
                            <Badge tone="warning">Excluído</Badge>
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
