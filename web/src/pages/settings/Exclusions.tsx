import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { SearchX, ShieldOff, X } from 'lucide-react'
import { useMemo, useState } from 'react'
import { toast } from 'sonner'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Skeleton } from '@/components/ui/misc'
import { library } from '@/lib/api'
import { formatCount } from '@/lib/format'

function normalize(text: string) {
  return text
    .normalize('NFD')
    .replace(/\p{Diacritic}/gu, '')
    .toLowerCase()
}

export function ExclusionsSection() {
  const queryClient = useQueryClient()
  const exclusions = useQuery({ queryKey: ['exclusoes'], queryFn: library.exclusions })
  const [query, setQuery] = useState('')

  const remove = useMutation({
    mutationFn: (tmdb: number) => library.removeExclusion(tmdb),
    onSuccess: (_, tmdb) => {
      queryClient.setQueryData(['exclusoes'], (current: typeof exclusions.data) =>
        current ? { exclusoes: current.exclusoes.filter((item) => item.tmdb_id !== tmdb) } : current,
      )
      toast.success('Filme tirado da exclusão')
    },
    onError: (err: Error) => toast.error(err.message),
  })

  const list = exclusions.data?.exclusoes ?? []
  const visible = useMemo(() => {
    const needle = normalize(query.trim())
    if (!needle) return list
    return list.filter((item) => normalize(`${item.title} ${item.year ?? ''} ${item.tmdb_id}`).includes(needle))
  }, [list, query])

  return (
    <section aria-labelledby="exclusoes-titulo" className="max-w-2xl rounded-lg border border-border bg-surface p-6">
      <div>
        <h2 id="exclusoes-titulo" className="flex items-center gap-2 text-lg font-semibold">
          <ShieldOff className="size-4.5 text-content-subtle" aria-hidden="true" />
          Exclusões
        </h2>
        <p className="mt-1 max-w-[60ch] text-sm text-content-muted">
          Filmes que nunca voltam ao catálogo por uma lista, mesmo que apareçam nela de novo.
        </p>
      </div>

      {exclusions.isPending ? (
        <div aria-busy="true" className="mt-6 grid gap-3">
          <Skeleton className="h-9 w-full" />
          <Skeleton className="h-4 w-40" />
          {Array.from({ length: 6 }, (_, key) => (
            <Skeleton key={key} className="h-10 w-full" />
          ))}
        </div>
      ) : exclusions.isError ? (
        <div role="alert" className="mt-6 flex flex-col items-start gap-3">
          <p className="text-sm text-danger">{exclusions.error.message}</p>
          <Button onClick={() => void exclusions.refetch()}>Tentar novamente</Button>
        </div>
      ) : list.length === 0 ? (
        <div className="mt-6 flex flex-col items-center gap-2 rounded-md border border-dashed border-border-strong px-6 py-10 text-center">
          <ShieldOff className="size-6 text-content-subtle" aria-hidden="true" />
          <p className="max-w-[45ch] text-sm text-content-muted">
            Nenhum filme excluído. Ao remover um filme você pode marcá-lo para nunca voltar por uma lista.
          </p>
        </div>
      ) : (
        <div className="mt-6 grid gap-3">
          <label>
            <span className="sr-only">Buscar exclusões</span>
            <Input
              type="search"
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              placeholder="Buscar por título, ano ou id do TMDB"
            />
          </label>
          <p className="text-xs text-content-subtle tabular-nums">
            {visible.length === list.length
              ? `${formatCount(list.length)} filmes excluídos`
              : `${formatCount(visible.length)} de ${formatCount(list.length)} filmes excluídos`}
          </p>

          {visible.length === 0 ? (
            <div className="flex flex-col items-center gap-2 rounded-md border border-dashed border-border-strong px-6 py-8 text-center">
              <SearchX className="size-5 text-content-subtle" aria-hidden="true" />
              <p className="text-sm text-content-muted">Nenhum filme encontrado com essa busca.</p>
            </div>
          ) : (
            <ul className="max-h-96 divide-y divide-border overflow-auto rounded-md border border-border">
              {visible.map((item) => (
                <li key={item.tmdb_id} className="flex items-center justify-between gap-3 px-3 py-2">
                  <p className="min-w-0 truncate text-sm">
                    {item.title}
                    {item.year ? ` (${item.year})` : ''}{' '}
                    <span className="text-xs text-content-subtle tabular-nums">#{item.tmdb_id}</span>
                  </p>
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon-sm"
                    aria-label="Tirar da exclusão"
                    loading={remove.isPending && remove.variables === item.tmdb_id}
                    onClick={() => remove.mutate(item.tmdb_id)}
                  >
                    {!(remove.isPending && remove.variables === item.tmdb_id) && (
                      <X className="size-4" aria-hidden="true" />
                    )}
                  </Button>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
    </section>
  )
}
