import { useMutation, useQueryClient } from '@tanstack/react-query'
import { ArrowLeft, Tv } from 'lucide-react'
import { useState } from 'react'
import { toast } from 'sonner'
import { Button } from '@/components/ui/button'
import { DialogFooter } from '@/components/ui/dialog'
import { Label } from '@/components/ui/label'
import { Checkbox } from '@/components/ui/misc'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { type SeriesTmdbResult, type WhatToSearch, seriesApi } from '@/lib/api'

const WHAT: { value: WhatToSearch; label: string }[] = [
  { value: 'tudo', label: 'Tudo que falta' },
  { value: 'ultima_temporada', label: 'A partir da última temporada' },
  { value: 'proximos', label: 'Só os próximos' },
]

export function AddSeriesChoose({
  result,
  onBack,
  onDone,
  backLabel = 'Voltar',
}: {
  result: SeriesTmdbResult
  onBack: () => void
  onDone: (id: number) => void
  backLabel?: string
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
          <Checkbox
            id="nova-monitorar"
            checked={monitorNew}
            onChange={(event) => setMonitorNew(event.target.checked)}
          />
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
          {backLabel}
        </Button>
        <Button variant="primary" loading={add.isPending} onClick={() => add.mutate()}>
          Adicionar série
        </Button>
      </DialogFooter>
    </>
  )
}
