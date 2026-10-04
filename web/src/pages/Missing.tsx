import { useQuery } from '@tanstack/react-query'
import { CircleCheck, LoaderCircle } from 'lucide-react'
import { useState } from 'react'
import { ItemDetails, type ItemRef } from '@/components/ItemDetails'
import { MediaThumb } from '@/components/MediaThumb'
import { PageHeader } from '@/components/PageHeader'
import { PriorityBadge } from '@/components/PriorityStar'
import { Button } from '@/components/ui/button'
import { Badge, Skeleton } from '@/components/ui/misc'
import { type MediaKind, type MissingItem, library } from '@/lib/api'
import { formatCount } from '@/lib/format'
import { episodeCode, formatAirDate } from '@/lib/seriesFormat'
import { Chip, ChipGroup } from '@/pages/Movies'

type Filter = 'todos' | MediaKind

const FILTERS: { value: Filter; label: string }[] = [
  { value: 'todos', label: 'Todos' },
  { value: 'filme', label: 'Filmes' },
  { value: 'serie', label: 'Séries' },
]

function describe(item: MissingItem) {
  if (item.tipo === 'filme') {
    return {
      kind: 'filme' as const,
      id: item.filme_id,
      title: item.filme,
      subtitle: item.ano ? String(item.ano) : 'Filme',
      key: `f${item.filme_id}`,
    }
  }
  return {
    kind: 'serie' as const,
    id: item.serie_id,
    title: item.serie,
    subtitle: `${episodeCode(item.temporada, item.numero)}${item.titulo ? ` · ${item.titulo}` : ''}`,
    key: `e${item.episodio_id}`,
  }
}

function MissingRow({ item, onOpen }: { item: MissingItem; onOpen: () => void }) {
  const { kind, title, subtitle } = describe(item)
  return (
    <button
      type="button"
      onClick={onOpen}
      className="flex w-full items-center gap-3 px-3 py-2.5 text-left transition-colors hover:bg-surface-raised focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring"
    >
      <MediaThumb poster={item.poster} kind={kind} />
      <div className="min-w-0 flex-1">
        <p className="truncate text-sm font-medium">{title}</p>
        <p className="truncate text-xs text-content-muted">{subtitle}</p>
        <div className="mt-1 flex flex-wrap items-center gap-1.5 sm:hidden">
          <Badges item={item} />
        </div>
      </div>
      <div className="hidden shrink-0 items-center gap-1.5 sm:flex">
        <Badges item={item} />
      </div>
      <time className="shrink-0 text-xs text-content-subtle tabular-nums" dateTime={item.data ?? undefined}>
        {formatAirDate(item.data)}
      </time>
    </button>
  )
}

function Badges({ item }: { item: MissingItem }) {
  return (
    <>
      {item.prioritario && <PriorityBadge />}
      {item.baixando && (
        <Badge tone="accent">
          <LoaderCircle className="animate-spin motion-reduce:animate-none" aria-hidden="true" />
          Baixando
        </Badge>
      )}
    </>
  )
}

export function MissingPage() {
  const [filter, setFilter] = useState<Filter>('todos')
  const [open, setOpen] = useState<ItemRef | null>(null)
  const missing = useQuery({
    queryKey: ['faltando', filter],
    queryFn: () => library.missing(filter === 'todos' ? undefined : filter),
  })

  return (
    <>
      <PageHeader
        title="Faltando"
        description={
          missing.isSuccess
            ? `${formatCount(missing.data.total)} ${missing.data.total === 1 ? 'item falta' : 'itens faltam'} no disco, filmes e episódios já lançados.`
            : 'Filmes e episódios que ainda não estão no disco.'
        }
      />
      <div className="mb-5">
        <ChipGroup label="Tipo" single>
          {FILTERS.map(({ value, label }) => (
            <Chip key={value} single active={filter === value} onClick={() => setFilter(value)}>
              {label}
            </Chip>
          ))}
        </ChipGroup>
      </div>

      {missing.isPending ? (
        <div className="grid gap-2" aria-busy="true" aria-label="Carregando o que falta">
          {Array.from({ length: 8 }, (_, key) => (
            <Skeleton key={key} className="h-16" />
          ))}
        </div>
      ) : missing.isError ? (
        <div role="alert" className="flex flex-col items-start gap-3 rounded-lg border border-border bg-surface p-6">
          <p className="font-medium">Não foi possível ler o que falta.</p>
          <p className="text-sm text-content-muted">{missing.error.message}</p>
          <Button onClick={() => void missing.refetch()}>Tentar novamente</Button>
        </div>
      ) : missing.data.itens.length === 0 ? (
        <div className="flex flex-col items-center gap-3 rounded-lg border border-dashed border-border-strong px-6 py-16 text-center">
          <CircleCheck className="size-8 text-content-subtle" aria-hidden="true" />
          <p className="font-medium">Nada faltando</p>
          <p className="max-w-md text-sm text-content-muted">
            {filter === 'todos'
              ? 'Tudo o que já foi lançado está no disco.'
              : `Nenhum ${filter === 'filme' ? 'filme' : 'episódio'} em falta.`}
          </p>
        </div>
      ) : (
        <ul className="divide-y divide-border overflow-hidden rounded-lg border border-border bg-surface" aria-label="Faltando">
          {missing.data.itens.map((item) => {
            const { kind, id, key } = describe(item)
            return (
              <li key={key}>
                <MissingRow item={item} onOpen={() => setOpen({ kind, id })} />
              </li>
            )
          })}
        </ul>
      )}

      {open && <ItemDetails item={open} onClose={() => setOpen(null)} />}
    </>
  )
}
