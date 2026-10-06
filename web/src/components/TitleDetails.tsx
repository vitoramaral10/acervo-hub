import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Plus, Star } from 'lucide-react'
import { toast } from 'sonner'
import { Button } from '@/components/ui/button'
import { Dialog, DialogContent, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { Skeleton } from '@/components/ui/misc'
import { ApiError, discover, type DiscoverItem, type DiscoverSearchItem } from '@/lib/api'
import { Poster } from '@/pages/Movies'

const queryKey = ['descobrir'] as const
const itemKey = (item: Pick<DiscoverItem, 'tipo' | 'tmdb'>) => `${item.tipo}-${item.tmdb}`
const SERIES_STATUS: Record<string, string> = {
  'Returning Series': 'Em andamento',
  Ended: 'Encerrada',
  Canceled: 'Cancelada',
  'In Production': 'Em produção',
  Planned: 'Planejada',
  Pilot: 'Piloto',
}
function ErrorState({ error, retry }: { error: Error; retry: () => void }) {
  return (
    <div role="alert" className="grid justify-items-start gap-3 rounded-md border border-border p-4">
      <p className="text-sm text-danger break-words">{error.message}</p>
      {error instanceof ApiError && error.status === 422 && /tmdb/i.test(error.message) && (
        <p className="text-sm text-content-muted">
          Configure a chave em{' '}
          <a href="#configuracoes?secao=tmdb" className="text-accent underline">
            Configurações → TMDB
          </a>
          .
        </p>
      )}
      <Button size="sm" onClick={retry}>
        Tentar novamente
      </Button>
    </div>
  )
}

export function TitleDetails({
  target,
  onClose,
  onSelect,
  onAdd,
}: {
  target: Pick<DiscoverItem, 'tipo' | 'tmdb'> | null
  onClose: () => void
  onSelect: (item: DiscoverItem) => void
  onAdd: (item: DiscoverItem) => void
}) {
  const client = useQueryClient()
  const details = useQuery({
    queryKey: [...queryKey, 'titulo', target?.tipo, target?.tmdb],
    queryFn: () => discover.details(target!.tipo, target!.tmdb),
    enabled: !!target,
  })
  const change = useMutation({
    mutationFn: (item: DiscoverSearchItem) =>
      item.oculto ? discover.showTitle(item.tipo, item.tmdb) : discover.hideTitle(item),
    onSuccess: () => {
      void client.invalidateQueries({ queryKey })
    },
    onError: (error) => toast.error(error.message),
  })
  const item = details.data
  return (
    <Dialog
      open={target !== null}
      onOpenChange={(open) => {
        if (!open) onClose()
      }}
    >
      <DialogContent key={target ? itemKey(target) : 'fechado'} className="max-w-4xl" aria-describedby={undefined}>
        <DialogHeader>
          <DialogTitle>{item?.titulo ?? 'Detalhes do título'}</DialogTitle>
        </DialogHeader>
        {details.isPending ? (
          <div role="status" aria-label="Carregando detalhes" aria-busy="true">
            <Skeleton className="h-64" />
          </div>
        ) : details.isError ? (
          <ErrorState error={details.error} retry={() => void details.refetch()} />
        ) : (
          item && (
            <>
              {item.backdrop && (
                <img src={item.backdrop} alt="" className="aspect-video max-h-72 w-full rounded-md object-cover" />
              )}
              <div className="flex items-start gap-4">
                <div className="w-24 shrink-0 sm:w-36">
                  <Poster movie={item} />
                </div>
                <div className="min-w-0 space-y-2">
                  <p className="text-sm text-content-muted">
                    {item.tipo === 'filme' ? 'Filme' : 'Série'} · {item.ano ?? '—'} ·{' '}
                    {item.generos.join(' · ') || 'Sem gênero'}
                  </p>
                  <p className="text-sm text-content-muted">
                    {item.tipo === 'filme'
                      ? item.duracao
                        ? `${item.duracao} min`
                        : 'Duração indisponível'
                      : `${item.temporadas ?? '—'} temporadas · ${item.episodios ?? '—'} episódios`}
                  </p>
                  {item.tipo === 'serie' && item.status && (
                    <p className="text-sm text-content-muted">Status: {SERIES_STATUS[item.status] ?? item.status}</p>
                  )}
                  <p className="flex items-center gap-1 text-sm">
                    <Star className="size-4" aria-hidden="true" />
                    {item.nota.toLocaleString('pt-BR', { maximumFractionDigits: 1 })}
                  </p>
                  {item.tagline && <p className="text-sm italic text-content-muted">{item.tagline}</p>}
                  {item.diretores.length > 0 && <p className="text-sm">Direção: {item.diretores.join(', ')}</p>}
                  {item.criadores.length > 0 && <p className="text-sm">Criação: {item.criadores.join(', ')}</p>}
                </div>
              </div>
              <p className="text-sm leading-relaxed text-content-muted">{item.sinopse || 'Sem sinopse.'}</p>
              <div className="flex flex-wrap items-center gap-2">
                {item.no_acervo && item.id_acervo != null ? (
                  <a
                    href={`#${item.tipo === 'filme' ? 'filmes' : 'series'}?id=${item.id_acervo}`}
                    className="rounded bg-accent-soft px-3 py-2 text-sm text-accent-soft-fg"
                    onClick={onClose}
                  >
                    No acervo
                  </a>
                ) : (
                  <Button onClick={() => onAdd(item)}>
                    <Plus aria-hidden="true" />
                    Adicionar
                  </Button>
                )}
                <Button variant="ghost" loading={change.isPending} onClick={() => change.mutate(item)}>
                  {item.oculto ? 'Mostrar de novo' : 'Ocultar título'}
                </Button>
                {item.trailer && (
                  <Button asChild>
                    <a
                      href={`https://www.youtube.com/watch?v=${encodeURIComponent(item.trailer)}`}
                      target="_blank"
                      rel="noopener noreferrer"
                    >
                      Ver trailer
                    </a>
                  </Button>
                )}
              </div>
              {item.elenco.length > 0 && (
                <section className="min-w-0 space-y-3" aria-label="Elenco">
                  <h3 className="font-semibold">Elenco</h3>
                  <div className="flex gap-3 overflow-x-auto pb-2">
                    {item.elenco.map((person, index) => (
                      <div key={`${person.nome}-${index}`} className="w-24 shrink-0 text-xs">
                        {person.foto ? (
                          <img
                            src={person.foto}
                            alt=""
                            loading="lazy"
                            className="aspect-[2/3] w-full rounded-md object-cover"
                          />
                        ) : (
                          <div className="aspect-[2/3] rounded-md bg-surface-raised" />
                        )}
                        <p className="mt-2 font-medium">{person.nome}</p>
                        <p className="text-content-subtle">{person.personagem}</p>
                      </div>
                    ))}
                  </div>
                </section>
              )}
              {item.recomendacoes.length > 0 && (
                <section className="min-w-0 space-y-3" aria-label="Recomendações">
                  <h3 className="font-semibold">Recomendações</h3>
                  <div className="flex gap-3 overflow-x-auto pb-2">
                    {item.recomendacoes.map((entry) => (
                      <button
                        key={itemKey(entry)}
                        type="button"
                        onClick={() => onSelect(entry)}
                        className="w-28 shrink-0 rounded-md text-left focus-visible:outline-2 focus-visible:outline-ring"
                      >
                        <Poster movie={entry} />
                        <span className="mt-2 block text-xs">{entry.titulo}</span>
                      </button>
                    ))}
                  </div>
                </section>
              )}
            </>
          )
        )}
      </DialogContent>
    </Dialog>
  )
}

