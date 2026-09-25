import { useMutation, useQueryClient } from '@tanstack/react-query'
import { ArrowDownToLine, CircleAlert, CircleCheck, ExternalLink, RefreshCw } from 'lucide-react'
import { useState } from 'react'
import { toast } from 'sonner'
import { Button } from '@/components/ui/button'
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { Badge, Skeleton, Tooltip } from '@/components/ui/misc'
import { type InteractiveRelease, type Movie, library } from '@/lib/api'
import { formatCount, formatSize, safeHref } from '@/lib/format'
import { cn } from '@/lib/utils'

function age(hours: number | null): string {
  if (hours == null) return '—'
  if (hours < 1) return `${Math.max(1, Math.round(hours * 60))} min`
  if (hours < 48) return `${Math.round(hours)} h`
  return `${Math.round(hours / 24)} d`
}

type Filter = 'aprovados' | 'todos'

function ReleaseRow({
  release,
  onGrab,
  grabbing,
}: {
  release: InteractiveRelease
  onGrab: () => void
  grabbing: boolean
}) {
  const link = safeHref(release.info)
  return (
    <li className={cn('grid gap-2 py-3 sm:grid-cols-[1fr_auto]', !release.aprovado && 'opacity-80')}>
      <div className="min-w-0">
        <p className="flex items-start gap-1.5 font-mono text-xs break-all">
          {release.aprovado ? (
            <CircleCheck className="mt-px size-3.5 shrink-0 text-success" aria-label="Aprovado" />
          ) : (
            <CircleAlert className="mt-px size-3.5 shrink-0 text-warning" aria-label="Recusado" />
          )}
          <span>{release.titulo}</span>
          {link && (
            <a href={link} target="_blank" rel="noreferrer" aria-label="Página do release no indexador">
              <ExternalLink className="size-3.5 text-content-subtle hover:text-content" aria-hidden="true" />
            </a>
          )}
        </p>
        <div className="mt-1.5 flex flex-wrap items-center gap-1.5">
          {release.qualidade && <Badge>{release.qualidade}</Badge>}
          {release.formatos.map((format) => (
            <Badge key={format} tone="accent">
              {format}
            </Badge>
          ))}
          {release.nota !== 0 && (
            <span className="text-xs text-content-muted tabular-nums">
              nota {release.nota > 0 ? '+' : ''}
              {release.nota}
            </span>
          )}
          {release.idiomas.length > 0 && (
            <span className="text-xs text-content-subtle">{release.idiomas.join(', ')}</span>
          )}
        </div>
        {!release.aprovado && release.motivos.length > 0 && (
          <ul className="mt-1.5 text-xs text-warning">
            {release.motivos.map((reason) => (
              <li key={reason}>{reason}</li>
            ))}
          </ul>
        )}
      </div>
      <div className="flex items-center gap-4 sm:flex-col sm:items-end sm:gap-1">
        <p className="text-xs text-content-muted tabular-nums">
          {formatSize(release.tamanho)} · {formatCount(release.seeders)} seeds · {age(release.idade_horas)}
        </p>
        <p className="text-xs text-content-subtle">{release.indexador}</p>
        <Tooltip content={release.aprovado ? 'Pegar este' : 'Pegar mesmo recusado'}>
          <Button
            size="sm"
            variant={release.aprovado ? 'primary' : undefined}
            loading={grabbing}
            onClick={onGrab}
            aria-label={`Pegar ${release.titulo}`}
          >
            {!grabbing && <ArrowDownToLine aria-hidden="true" />}
            Pegar
          </Button>
        </Tooltip>
      </div>
    </li>
  )
}

/** Busca em todos os indexadores e mostra cada release com a decisão; o grab é de quem escolhe. */
export function InteractiveSearch({
  movie,
  open,
  onOpenChange,
}: {
  movie: Movie
  open: boolean
  onOpenChange: (open: boolean) => void
}) {
  const queryClient = useQueryClient()
  const [filter, setFilter] = useState<Filter>('aprovados')
  const search = useMutation({ mutationFn: () => library.releases(movie.id) })
  const grab = useMutation({
    mutationFn: (guid: string) => library.grabRelease(movie.id, guid),
    onSuccess: ({ titulo }) => {
      toast.success(`Mandado ao qBittorrent: ${titulo}`)
      void queryClient.invalidateQueries({ queryKey: ['filmes'] })
      void queryClient.invalidateQueries({ queryKey: ['fila'] })
      onOpenChange(false)
    },
    onError: (error: Error) => toast.error(error.message),
  })

  const releases = search.data?.releases ?? []
  const approved = releases.filter((r) => r.aprovado)
  const own = releases.filter((r) => !r.outro_filme)
  const shown = filter === 'aprovados' ? approved : own

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        onOpenChange(next)
        if (next) {
          setFilter('aprovados')
          search.mutate()
        }
      }}
    >
      <DialogContent className="max-w-4xl">
        <DialogHeader>
          <DialogTitle>
            Busca interativa: {movie.titulo}
            {movie.ano ? ` (${movie.ano})` : ''}
          </DialogTitle>
          <DialogDescription>
            Todos os releases dos indexadores, na ordem em que o acervo-hub os escolheria, com o motivo de cada recusa.
          </DialogDescription>
        </DialogHeader>
        <div className="flex flex-wrap items-center justify-between gap-3">
          <div role="radiogroup" aria-label="Mostrar" className="flex rounded-md border border-border p-0.5">
            {(
              [
                ['aprovados', `Aprovados (${approved.length})`],
                ['todos', `Todos deste filme (${own.length})`],
              ] as const
            ).map(([value, label]) => (
              <button
                key={value}
                type="button"
                role="radio"
                aria-checked={filter === value}
                onClick={() => setFilter(value)}
                className={cn(
                  'rounded-sm px-2.5 py-1.5 text-xs font-medium text-content-muted transition-colors hover:text-content focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-ring',
                  filter === value && 'bg-surface-raised text-content',
                )}
              >
                {label}
              </button>
            ))}
          </div>
          <Button variant="ghost" size="sm" loading={search.isPending} onClick={() => search.mutate()}>
            {!search.isPending && <RefreshCw aria-hidden="true" />}
            Buscar de novo
          </Button>
        </div>
        <div className="-mx-2 max-h-[60vh] min-h-40 overflow-y-auto px-2" aria-live="polite">
          {search.isPending ? (
            <div className="grid gap-2" aria-busy="true">
              {[0, 1, 2, 3].map((key) => (
                <Skeleton key={key} className="h-16" />
              ))}
              <p className="text-xs text-content-subtle">Buscando nos indexadores…</p>
            </div>
          ) : search.isError ? (
            <p role="alert" className="text-sm text-danger">
              {search.error.message}
            </p>
          ) : shown.length === 0 ? (
            <p className="py-10 text-center text-sm text-content-muted">
              {filter === 'aprovados' && own.length > 0
                ? 'Nenhum release aprovado. Veja todos para saber por que cada um foi recusado.'
                : 'Nenhum release deste filme nos indexadores.'}
            </p>
          ) : (
            <ul className="divide-y divide-border">
              {shown.map((release) => (
                <ReleaseRow
                  key={release.guid}
                  release={release}
                  grabbing={grab.isPending && grab.variables === release.guid}
                  onGrab={() => grab.mutate(release.guid)}
                />
              ))}
            </ul>
          )}
        </div>
      </DialogContent>
    </Dialog>
  )
}
