import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { CircleAlert, Eye, ListPlus, Trash2, Undo2, X } from 'lucide-react'
import { useEffect, useMemo, useRef, useState } from 'react'
import { toast } from 'sonner'
import { ConfirmButton } from '@/components/ConfirmButton'
import { DELETION_KEY, useMarkedKeys, useToggleMark } from '@/components/DeletionMark'
import { ItemDetails, type ItemRef } from '@/components/ItemDetails'
import { MediaThumb } from '@/components/MediaThumb'
import { PageHeader } from '@/components/PageHeader'
import { Button } from '@/components/ui/button'
import { Checkbox, Skeleton } from '@/components/ui/misc'
import {
  type DeletionItem,
  type MarkedItem,
  type PurgeResult,
  type SuggestedItem,
  deletionApi,
  deletionTarget,
} from '@/lib/api'
import { formatCount, formatSize } from '@/lib/format'

const SUGGESTIONS_KEY = ['para-apagar-sugestoes'] as const

const plural = (n: number, one: string, many: string) => `${formatCount(n)} ${n === 1 ? one : many}`
const sum = (items: DeletionItem[]) => items.reduce((total, item) => total + item.tamanho, 0)

/** O detalhe do item: o filme abre o dele; temporada e série, o da série. */
function itemRef(item: DeletionItem): ItemRef {
  return item.tipo === 'filme' ? { kind: 'filme', id: item.filme as number } : { kind: 'serie', id: item.serie as number }
}

function describe(item: DeletionItem) {
  const parts = [item.detalhe ?? 'Filme', plural(item.arquivos, 'arquivo', 'arquivos')]
  return parts.join(' · ')
}

function ItemTitle({ item, onOpen }: { item: DeletionItem; onOpen: () => void }) {
  return (
    <button
      type="button"
      onClick={onOpen}
      className="block max-w-full truncate rounded-sm text-left text-sm font-medium hover:text-accent focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring"
      title={item.titulo}
    >
      {item.titulo}
    </button>
  )
}

function Problem({ title, message, onRetry }: { title: string; message: string; onRetry: () => void }) {
  return (
    <div role="alert" className="flex flex-col items-start gap-3 rounded-lg border border-border bg-surface p-6">
      <p className="font-medium">{title}</p>
      <p className="text-sm text-content-muted">{message}</p>
      <Button onClick={onRetry}>Tentar novamente</Button>
    </div>
  )
}

function ListSkeleton({ rows, label }: { rows: number; label: string }) {
  return (
    <div className="grid gap-2" aria-busy="true" aria-label={label}>
      {Array.from({ length: rows }, (_, key) => (
        <Skeleton key={key} className="h-16" />
      ))}
    </div>
  )
}

/** O que saiu, o que falhou e o que ficou pendente na última confirmação. */
function PurgeReport({ result, onDismiss }: { result: PurgeResult; onDismiss: () => void }) {
  if (result.falhas.length === 0 && result.avisos.length === 0) return null
  return (
    <div role="status" className="mb-6 rounded-lg border border-border bg-surface p-4">
      <div className="flex items-start justify-between gap-3">
        <p className="flex items-center gap-2 text-sm font-medium">
          <CircleAlert className="size-4 text-warning" aria-hidden="true" />
          {plural(result.apagados.length, 'item apagado', 'itens apagados')},{' '}
          {plural(result.falhas.length, 'falha', 'falhas')}
        </p>
        <Button variant="ghost" size="icon-sm" onClick={onDismiss} aria-label="Fechar o resultado">
          <X aria-hidden="true" />
        </Button>
      </div>
      {result.falhas.length > 0 && (
        <ul className="mt-3 grid gap-1.5 text-sm">
          {result.falhas.map((failure) => (
            <li key={failure.chave} className="break-words">
              <span className="font-medium">{failure.titulo}</span>
              <span className="text-danger"> — {failure.erro}</span>
            </li>
          ))}
        </ul>
      )}
      {result.avisos.length > 0 && (
        <ul className="mt-3 grid gap-1.5 text-xs text-content-muted">
          {result.avisos.map((warning) => (
            <li key={warning} className="break-words">
              {warning}
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}

function MarkedSection({ onOpen }: { onOpen: (item: ItemRef) => void }) {
  const queryClient = useQueryClient()
  const marked = useQuery({ queryKey: DELETION_KEY, queryFn: deletionApi.list })
  const [selection, setSelection] = useState<Set<string>>(new Set())
  const [result, setResult] = useState<PurgeResult | null>(null)
  const toggle = useToggleMark()

  const items = useMemo(() => marked.data?.itens ?? [], [marked.data])
  const selected = useMemo(() => items.filter((item) => selection.has(item.chave)), [items, selection])
  const all = useRef<HTMLInputElement>(null)
  useEffect(() => {
    if (all.current) all.current.indeterminate = selected.length > 0 && selected.length < items.length
  }, [selected.length, items.length])

  const purge = useMutation({
    mutationFn: (targets: MarkedItem[]) => deletionApi.purge(targets.map(deletionTarget)),
    onSuccess: (outcome) => {
      setResult(outcome)
      setSelection(new Set())
      if (outcome.apagados.length > 0) {
        toast.success(
          `${plural(outcome.apagados.length, 'item apagado', 'itens apagados')}, ${formatSize(outcome.liberado)} liberados`,
        )
      }
      if (outcome.falhas.length > 0) toast.error(plural(outcome.falhas.length, 'item não saiu', 'itens não saíram'))
    },
    onError: (error: Error) => toast.error(error.message),
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: DELETION_KEY })
      void queryClient.invalidateQueries({ queryKey: SUGGESTIONS_KEY })
      void queryClient.invalidateQueries({ queryKey: ['filmes'] })
      void queryClient.invalidateQueries({ queryKey: ['series'] })
      void queryClient.invalidateQueries({ queryKey: ['fila'] })
    },
  })
  const unmarkSelected = () =>
    toggle.mutate(
      { targets: selected.map(deletionTarget), mark: false, label: plural(selected.length, 'item', 'itens') },
      { onSuccess: () => setSelection(new Set()) },
    )

  const busy = purge.isPending || toggle.isPending
  const confirmText = (list: MarkedItem[]) =>
    `Sai do disco ${formatSize(sum(list))}, em ${plural(list.length, 'item', 'itens')}. ` +
    'Filme sai do catálogo com a pasta e o download; temporada e série perdem os arquivos e os episódios ficam fora da busca. Não dá para desfazer.'

  return (
    <section aria-labelledby="marcados-titulo" className="mb-12">
      <div className="mb-3 flex flex-wrap items-baseline justify-between gap-2">
        <h2 id="marcados-titulo" className="text-lg font-semibold tracking-tight">
          Marcados
        </h2>
        {marked.isSuccess && items.length > 0 && (
          <p className="text-sm text-content-muted tabular-nums">
            {plural(items.length, 'item', 'itens')} · <span className="font-medium text-content">{formatSize(marked.data.total)}</span>{' '}
            no total
          </p>
        )}
      </div>

      {result && <PurgeReport result={result} onDismiss={() => setResult(null)} />}

      {marked.isPending ? (
        <ListSkeleton rows={3} label="Carregando os marcados" />
      ) : marked.isError ? (
        <Problem
          title="Não foi possível ler os marcados."
          message={marked.error.message}
          onRetry={() => void marked.refetch()}
        />
      ) : items.length === 0 ? (
        <div className="flex flex-col items-center gap-3 rounded-lg border border-dashed border-border-strong px-6 py-12 text-center">
          <Trash2 className="size-8 text-content-subtle" aria-hidden="true" />
          <p className="font-medium">Nada marcado</p>
          <p className="max-w-md text-sm text-content-muted">
            Marque filmes, temporadas ou séries inteiras no detalhe de cada um, ou pelas sugestões abaixo. Nada sai
            do disco antes de confirmar aqui.
          </p>
        </div>
      ) : (
        <>
          <div className="mb-3 flex flex-wrap items-center gap-2">
            <ConfirmButton
              variant="danger"
              size="sm"
              disabled={busy}
              title={`Apagar ${plural(items.length, 'item marcado', 'itens marcados')}?`}
              description={confirmText(items)}
              confirmLabel={`Apagar ${formatSize(marked.data.total)}`}
              onConfirm={() => purge.mutate(items)}
            >
              <Trash2 aria-hidden="true" />
              Apagar todos
            </ConfirmButton>
            <ConfirmButton
              variant="secondary"
              size="sm"
              disabled={busy || selected.length === 0}
              title={`Apagar ${plural(selected.length, 'item selecionado', 'itens selecionados')}?`}
              description={confirmText(selected)}
              confirmLabel={`Apagar ${formatSize(sum(selected))}`}
              onConfirm={() => purge.mutate(selected)}
            >
              <Trash2 className="text-danger" aria-hidden="true" />
              Apagar selecionados
              {selected.length > 0 && <span className="tabular-nums">({formatSize(sum(selected))})</span>}
            </ConfirmButton>
            <Button size="sm" variant="ghost" disabled={busy || selected.length === 0} onClick={unmarkSelected}>
              <Undo2 aria-hidden="true" />
              Desmarcar selecionados
            </Button>
            {purge.isPending && <span className="text-sm text-content-muted">Apagando…</span>}
          </div>
          <ul
            className="divide-y divide-border overflow-hidden rounded-lg border border-border bg-surface"
            aria-label="Marcados para apagar"
            aria-busy={purge.isPending}
          >
            <li className="flex items-center gap-3 bg-surface-raised/50 px-3 py-2 text-xs text-content-muted">
              <Checkbox
                ref={all}
                checked={selected.length > 0 && selected.length === items.length}
                onChange={(event) => setSelection(event.target.checked ? new Set(items.map((i) => i.chave)) : new Set())}
                aria-label="Selecionar todos os marcados"
              />
              <span>Selecionar todos</span>
            </li>
            {items.map((item) => (
              <li key={item.chave} className="flex flex-wrap items-center gap-x-3 gap-y-2 px-3 py-2.5">
                <Checkbox
                  checked={selection.has(item.chave)}
                  onChange={(event) =>
                    setSelection((old) => {
                      const next = new Set(old)
                      if (event.target.checked) next.add(item.chave)
                      else next.delete(item.chave)
                      return next
                    })
                  }
                  aria-label={`Selecionar ${item.titulo}${item.detalhe ? `, ${item.detalhe}` : ''}`}
                />
                <MediaThumb poster={item.poster} kind={item.tipo === 'filme' ? 'filme' : 'serie'} />
                <div className="min-w-0 flex-1 basis-40">
                  <ItemTitle item={item} onOpen={() => onOpen(itemRef(item))} />
                  <p className="truncate text-xs text-content-muted">{describe(item)}</p>
                </div>
                <span className="ml-auto text-sm font-medium tabular-nums">{formatSize(item.tamanho)}</span>
                <Button
                  size="sm"
                  variant="ghost"
                  disabled={busy}
                  onClick={() =>
                    toggle.mutate({ targets: [deletionTarget(item)], mark: false, label: item.detalhe ?? item.titulo })
                  }
                >
                  <Undo2 aria-hidden="true" />
                  Desmarcar
                </Button>
              </li>
            ))}
          </ul>
        </>
      )}
    </section>
  )
}

function SuggestionsSection({ onOpen }: { onOpen: (item: ItemRef) => void }) {
  const suggestions = useQuery({
    queryKey: SUGGESTIONS_KEY,
    queryFn: deletionApi.suggestions,
    // Lê o Jellyfin a cada vez: não precisa repetir a cada foco da janela.
    staleTime: 60_000,
  })
  const marked = useMarkedKeys()
  const toggle = useToggleMark()
  // O que acabou de ser marcado some daqui sem esperar a próxima leitura do Jellyfin.
  const items: SuggestedItem[] = useMemo(
    () =>
      suggestions.data
        ? [...suggestions.data.filmes, ...suggestions.data.temporadas].filter((item) => !marked.has(item.chave))
        : [],
    [suggestions.data, marked],
  )
  const markAll = () =>
    toggle.mutate({
      targets: items.map(deletionTarget),
      mark: true,
      label: plural(items.length, 'sugestão', 'sugestões'),
    })

  return (
    <section aria-labelledby="sugestoes-titulo">
      <div className="mb-1 flex flex-wrap items-center justify-between gap-2">
        <h2 id="sugestoes-titulo" className="text-lg font-semibold tracking-tight">
          Sugestões
        </h2>
        {items.length > 1 && (
          <Button size="sm" disabled={toggle.isPending} onClick={markAll}>
            <ListPlus aria-hidden="true" />
            Marcar todas ({formatSize(sum(items))})
          </Button>
        )}
      </div>
      <p className="mb-4 max-w-[65ch] text-sm text-content-muted">
        O que alguém já assistiu no Jellyfin, passada a carência, que ninguém marcou como favorito. Séries entram por
        temporada, quando todos os episódios dela no disco foram assistidos. Marcar não apaga.
      </p>

      {suggestions.isPending ? (
        <ListSkeleton rows={3} label="Lendo o Jellyfin" />
      ) : suggestions.isError ? (
        <Problem
          title="Não foi possível ler o que foi assistido."
          message={suggestions.error.message}
          onRetry={() => void suggestions.refetch()}
        />
      ) : !suggestions.data.configurado ? (
        <div className="rounded-lg border border-dashed border-border-strong px-6 py-10 text-center text-sm text-content-muted">
          Sem Jellyfin configurado, não há sugestões. Configure em{' '}
          <a href="#jellyfin" className="font-medium text-accent hover:underline">
            Configurações → Jellyfin
          </a>
          .
        </div>
      ) : items.length === 0 ? (
        <div className="flex flex-col items-center gap-3 rounded-lg border border-dashed border-border-strong px-6 py-12 text-center">
          <Eye className="size-8 text-content-subtle" aria-hidden="true" />
          <p className="font-medium">Nenhuma sugestão</p>
          <p className="max-w-md text-sm text-content-muted">Nada assistido além da carência ficou de fora da lista.</p>
        </div>
      ) : (
        <ul className="divide-y divide-border overflow-hidden rounded-lg border border-border bg-surface" aria-label="Sugestões">
          {items.map((item) => (
            <li key={item.chave} className="flex flex-wrap items-center gap-x-3 gap-y-2 px-3 py-2.5">
              <MediaThumb poster={item.poster} kind={item.tipo === 'filme' ? 'filme' : 'serie'} />
              <div className="min-w-0 flex-1 basis-40">
                <ItemTitle item={item} onOpen={() => onOpen(itemRef(item))} />
                <p className="truncate text-xs text-content-muted">
                  {describe(item)} · assistido por {item.assistido_por} em{' '}
                  <time dateTime={item.assistido_em}>{new Date(item.assistido_em).toLocaleDateString('pt-BR')}</time>
                </p>
              </div>
              <span className="ml-auto text-sm font-medium tabular-nums">{formatSize(item.tamanho)}</span>
              <Button
                size="sm"
                disabled={toggle.isPending}
                onClick={() =>
                  toggle.mutate({
                    targets: [deletionTarget(item)],
                    mark: true,
                    label: item.detalhe ? `${item.titulo}, ${item.detalhe}` : item.titulo,
                  })
                }
              >
                <Trash2 aria-hidden="true" />
                Marcar
              </Button>
            </li>
          ))}
        </ul>
      )}
    </section>
  )
}

export function ToDeletePage() {
  const [open, setOpen] = useState<ItemRef | null>(null)
  return (
    <>
      <PageHeader
        title="Para apagar"
        description="O que você marcou para tirar do disco, com o espaço que cada item libera. Nada sai antes de confirmar aqui; a limpeza de seeds segue sozinha, como sempre."
      />
      <MarkedSection onOpen={setOpen} />
      <SuggestionsSection onOpen={setOpen} />
      {open && <ItemDetails item={open} onClose={() => setOpen(null)} />}
    </>
  )
}
