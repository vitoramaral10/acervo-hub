import { useInfiniteQuery, useQueryClient } from '@tanstack/react-query'
import { Search } from 'lucide-react'
import { useEffect, useRef, useState } from 'react'
import { AddTitleDialog } from '@/components/AddTitleDialog'
import { TitleDetails } from '@/components/TitleDetails'
import { Button } from '@/components/ui/button'
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Badge } from '@/components/ui/misc'
import { discover, type DiscoverItem } from '@/lib/api'

const key = (item: Pick<DiscoverItem, 'tipo' | 'tmdb'>) => `${item.tipo}-${item.tmdb}`

export function GlobalSearch() {
  const client = useQueryClient()
  const [open, setOpen] = useState(false)
  const [term, setTerm] = useState('')
  const [query, setQuery] = useState('')
  const [target, setTarget] = useState<Pick<DiscoverItem, 'tipo' | 'tmdb'> | null>(null)
  const [chosen, setChosen] = useState<DiscoverItem | null>(null)
  const trigger = useRef<HTMLButtonElement>(null)
  const input = useRef<HTMLInputElement>(null)

  useEffect(() => {
    const timer = window.setTimeout(() => setQuery(term.trim()), 350)
    return () => window.clearTimeout(timer)
  }, [term])
  useEffect(() => {
    const shortcut = (event: KeyboardEvent) => {
      if (event.defaultPrevented || event.isComposing || event.repeat) return
      const command = (event.ctrlKey || event.metaKey) && !event.altKey && event.key.toLowerCase() === 'k'
      const slash = event.key === '/' && !event.ctrlKey && !event.metaKey && !event.altKey
      const editing = event.target instanceof HTMLElement &&
        (event.target.isContentEditable || !!event.target.closest('input, textarea, select, [contenteditable="true"]'))
      if (!command && (!slash || editing)) return
      if (target || chosen || (!open && document.querySelector('[role="dialog"]'))) return
      event.preventDefault()
      if (open) input.current?.focus()
      else setOpen(true)
    }
    window.addEventListener('keydown', shortcut)
    return () => window.removeEventListener('keydown', shortcut)
  }, [open, target, chosen])

  const results = useInfiniteQuery({
    queryKey: ['descobrir', 'busca', query],
    initialPageParam: 1,
    queryFn: ({ pageParam }) => discover.search(query, pageParam),
    getNextPageParam: (page) => page.pagina < Math.min(page.total_paginas, 500) ? page.pagina + 1 : undefined,
    enabled: open && query.length >= 2 && query === term.trim(),
  })
  const items = [...new Map(results.data?.pages.flatMap((page) => page.itens).map((item) => [key(item), item])).values()]
  const closeDetails = () => {
    setTarget(null)
    setChosen(null)
    window.requestAnimationFrame(() => trigger.current?.focus())
  }
  const added = () => {
    closeDetails()
    void client.invalidateQueries({ queryKey: ['descobrir'] })
    void client.invalidateQueries({ queryKey: ['filmes'] })
    void client.invalidateQueries({ queryKey: ['series'] })
  }
  const waiting = term.trim() !== query || results.isPending

  return (
    <>
      <Button ref={trigger} size="sm" variant="secondary" onClick={() => setOpen(true)}
        className="shrink-0 md:mb-3 md:justify-start" aria-label="Buscar filmes e séries (barra ou Control/Command K)"
        aria-haspopup="dialog" aria-expanded={open} aria-controls="busca-global">
        <Search aria-hidden="true" />
        <span className="hidden md:inline">Buscar títulos</span>
        <kbd className="ml-auto hidden text-xs text-content-subtle md:inline">/</kbd>
      </Button>
      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent id="busca-global" className="max-w-2xl"
          onOpenAutoFocus={(event) => { event.preventDefault(); input.current?.focus() }}
          onCloseAutoFocus={(event) => {
            event.preventDefault()
            if (!target && !chosen) trigger.current?.focus()
          }}>
          <DialogHeader>
            <DialogTitle>Buscar filmes e séries</DialogTitle>
            <DialogDescription>Encontre um título para adicionar ou abrir no acervo.</DialogDescription>
          </DialogHeader>
          <Input ref={input} type="search" maxLength={100} value={term} aria-label="Título do filme ou série"
            aria-describedby="busca-global-status" placeholder="Título do filme ou série…"
            onChange={(event) => setTerm(event.target.value)} />
          <p id="busca-global-status" role="status" className="text-sm text-content-muted">
            {term.trim().length < 2 ? 'Digite pelo menos 2 caracteres.' : waiting ? 'Buscando…' :
              items.length === 0 ? 'Nenhum título encontrado.' : `${items.length} títulos carregados.`}
          </p>
          {term.trim().length >= 2 && query === term.trim() && (
            <div className="min-w-0 space-y-3" aria-busy={results.isFetching}>
              {results.isError && (
                <div role="alert" className="space-y-2 text-sm text-danger">
                  <p>{results.error.message}</p>
                  <Button size="sm" onClick={() => void (results.isFetchNextPageError ? results.fetchNextPage() : results.refetch())}>
                    Tentar novamente
                  </Button>
                </div>
              )}
              <ul className="max-h-[50dvh] space-y-1 overflow-y-auto" aria-label="Resultados da busca">
                {items.map((item) => (
                  <li key={key(item)}>
                    <button type="button" onClick={() => { setTarget(item); setOpen(false) }}
                      className="flex w-full items-center gap-3 rounded-md p-2 text-left hover:bg-surface-raised focus-visible:outline-2 focus-visible:outline-ring"
                      aria-label={`Ver detalhes de ${item.titulo}, ${item.tipo === 'filme' ? 'filme' : 'série'}${item.no_acervo ? ', no acervo' : ''}`}>
                      {item.poster ? <img src={item.poster} alt="" loading="lazy" className="h-16 w-11 shrink-0 rounded object-cover" /> :
                        <div className="h-16 w-11 shrink-0 rounded bg-surface-raised" />}
                      <span className="min-w-0 flex-1">
                        <span className="block font-medium">{item.titulo}</span>
                        <span className="text-xs text-content-muted">{item.ano ?? 'Ano desconhecido'}</span>
                        <span className="mt-1 flex flex-wrap gap-1">
                          <Badge>{item.tipo === 'filme' ? 'Filme' : 'Série'}</Badge>
                          {item.no_acervo && <Badge tone="success">No acervo</Badge>}
                        </span>
                      </span>
                    </button>
                  </li>
                ))}
              </ul>
              {results.hasNextPage && <Button loading={results.isFetchingNextPage} disabled={results.isFetching}
                onClick={() => void results.fetchNextPage()}>Carregar mais</Button>}
            </div>
          )}
        </DialogContent>
      </Dialog>
      <TitleDetails target={target} onClose={closeDetails} onSelect={setTarget}
        onAdd={(item) => { setTarget(null); setChosen(item) }} />
      <AddTitleDialog chosen={chosen} onClose={closeDetails} onDone={added} />
    </>
  )
}
