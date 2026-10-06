import { AddMovieChoose } from '@/components/AddMovieChoose'
import { AddSeriesChoose } from '@/components/AddSeriesChoose'
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { type DiscoverItem, type TmdbResult } from '@/lib/api'

export function AddTitleDialog({ chosen, onClose, onDone }: {
  chosen: DiscoverItem | null
  onClose: () => void
  onDone: () => void
}) {
  const result: TmdbResult | null = chosen ? {
    tmdb: chosen.tmdb, titulo: chosen.titulo, titulo_original: chosen.titulo_original,
    ano: chosen.ano, sinopse: chosen.sinopse, poster: chosen.poster, nota: chosen.nota, no_catalogo: null,
  } : null
  return (
    <Dialog open={chosen !== null} onOpenChange={(open) => { if (!open) onClose() }}>
      <DialogContent className="max-w-2xl">
        <DialogHeader>
          <DialogTitle>Adicionar {chosen?.tipo === 'serie' ? 'série' : 'filme'}</DialogTitle>
          <DialogDescription>Escolha como adicionar este título ao acervo.</DialogDescription>
        </DialogHeader>
        {chosen?.tipo === 'filme' && result && (
          <AddMovieChoose key={`filme-${chosen.tmdb}`} result={result} onBack={onClose} onDone={onDone} backLabel="Cancelar" />
        )}
        {chosen?.tipo === 'serie' && result && (
          <AddSeriesChoose key={`serie-${chosen.tmdb}`} result={result} onBack={onClose} onDone={onDone} backLabel="Cancelar" />
        )}
      </DialogContent>
    </Dialog>
  )
}
