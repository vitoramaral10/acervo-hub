import { useQuery } from '@tanstack/react-query'
import { SeriesDetails } from '@/components/SeriesDetails'
import { type MediaKind, api } from '@/lib/api'
import { MovieDetails } from '@/pages/Movies'

export type ItemRef = { kind: MediaKind; id: number }

/** Abre o detalhe de um filme ou de uma série a partir de uma lista que só tem o id. */
export function ItemDetails({ item, onClose }: { item: ItemRef; onClose: () => void }) {
  if (item.kind === 'serie') return <SeriesDetails id={item.id} onClose={onClose} />
  return <MovieItem id={item.id} onClose={onClose} />
}

function MovieItem({ id, onClose }: { id: number; onClose: () => void }) {
  const movies = useQuery({ queryKey: ['filmes'], queryFn: api.movies })
  const movie = movies.data?.filmes.find((candidate) => candidate.id === id)
  if (!movie) return null
  return <MovieDetails movie={movie} open onOpenChange={(open) => !open && onClose()} />
}
