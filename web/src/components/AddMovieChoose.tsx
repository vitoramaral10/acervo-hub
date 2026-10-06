import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { ArrowLeft, Film } from 'lucide-react'
import { useEffect, useState } from 'react'
import { toast } from 'sonner'
import { Button } from '@/components/ui/button'
import { DialogFooter } from '@/components/ui/dialog'
import { Label } from '@/components/ui/label'
import { Skeleton, Switch } from '@/components/ui/misc'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { type TmdbResult, library } from '@/lib/api'
import { formatSize } from '@/lib/format'

export function AddMovieChoose({
  result,
  onBack,
  onDone,
  backLabel = 'Voltar',
}: {
  result: TmdbResult
  onBack: () => void
  onDone: (id: number) => void
  backLabel?: string
}) {
  const queryClient = useQueryClient()
  const options = useQuery({ queryKey: ['biblioteca-opcoes'], queryFn: library.options })
  const [folder, setFolder] = useState('')
  const [monitored, setMonitored] = useState(true)
  const [search, setSearch] = useState(true)

  useEffect(() => {
    if (!options.data) return
    setFolder((current) => current || options.data.pastas[0]?.caminho || '')
  }, [options.data])

  const add = useMutation({
    mutationFn: () =>
      library.add({
        tmdb: result.tmdb,
        pasta: folder,
        monitorado: monitored,
        buscar: search && monitored,
      }),
    onSuccess: ({ id }) => {
      toast.success(`${result.titulo} adicionado${search && monitored ? ' — buscando' : ''}`)
      void queryClient.invalidateQueries({ queryKey: ['filmes'] })
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
          {backLabel}
        </Button>
        <Button variant="primary" disabled={!folder} loading={add.isPending} onClick={() => add.mutate()}>
          Adicionar filme
        </Button>
      </DialogFooter>
    </>
  )
}
