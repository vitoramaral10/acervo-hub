import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Film, ListPlus, Pencil, RefreshCw, SearchX, Trash2 } from 'lucide-react'
import { useState } from 'react'
import { toast } from 'sonner'
import { ConfirmButton } from '@/components/ConfirmButton'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Badge, Skeleton, Switch } from '@/components/ui/misc'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { type ImportList, type ImportListInput, type ImportListKind, type LibraryOptions, library } from '@/lib/api'
import { formatAgo } from '@/lib/format'
import { cn } from '@/lib/utils'

const KIND_LABELS: Record<ImportListKind, string> = {
  tmdb_person: 'Pessoa do TMDB',
  tmdb_collection: 'Coleção do TMDB',
  tmdb_list: 'Lista do TMDB',
}

const AVAILABILITY: { value: string; label: string }[] = [
  { value: 'announced', label: 'Anunciado' },
  { value: 'inCinemas', label: 'No cinema' },
  { value: 'released', label: 'Lançado' },
]

const DEPARTMENTS: { value: string; label: string }[] = [
  { value: 'Directing', label: 'Direção' },
  { value: 'Writing', label: 'Roteiro' },
  { value: 'Production', label: 'Produção' },
  { value: 'Sound', label: 'Som' },
]

interface EditorState {
  nome: string
  tipo: ImportListKind
  pessoa: string
  elenco: boolean
  departamentos: string[]
  colecao: string
  lista: string
  perfil: string
  pasta: string
  disponibilidade: string
  ligada: boolean
  monitorar: boolean
  buscarAoAdicionar: boolean
  tags: number[]
}

function emptyEditor(options: LibraryOptions | undefined): EditorState {
  return {
    nome: '',
    tipo: 'tmdb_person',
    pessoa: '',
    elenco: true,
    departamentos: [],
    colecao: '',
    lista: '',
    perfil: options?.perfis[0] ? String(options.perfis[0].id) : '',
    pasta: options?.pastas[0]?.caminho ?? '',
    disponibilidade: 'released',
    ligada: true,
    monitorar: true,
    buscarAoAdicionar: true,
    tags: [],
  }
}

function editorFromList(list: ImportList): EditorState {
  const settings = list.settings as Record<string, unknown>
  return {
    nome: list.name,
    tipo: list.kind,
    pessoa: settings.pessoa != null ? String(settings.pessoa) : '',
    elenco: settings.elenco !== false,
    departamentos: Array.isArray(settings.departamentos) ? (settings.departamentos as string[]) : [],
    colecao: settings.colecao != null ? String(settings.colecao) : '',
    lista: typeof settings.lista === 'string' ? settings.lista : '',
    perfil: list.quality_profile_id != null ? String(list.quality_profile_id) : '',
    pasta: list.root_folder,
    disponibilidade: list.minimum_availability,
    ligada: list.enabled,
    monitorar: list.monitor,
    buscarAoAdicionar: list.search_on_add,
    tags: list.tags,
  }
}

function buildInput(state: EditorState): ImportListInput | string {
  if (!state.nome.trim()) return 'Dê um nome para a lista.'
  if (!state.perfil) return 'Escolha um perfil de qualidade.'
  if (!state.pasta) return 'Escolha uma pasta.'
  let configuracao: Record<string, unknown>
  if (state.tipo === 'tmdb_person') {
    const pessoa = Number(state.pessoa)
    if (!pessoa) return 'Informe o id da pessoa no TMDB.'
    configuracao = { pessoa, elenco: state.elenco, departamentos: state.departamentos }
  } else if (state.tipo === 'tmdb_collection') {
    const colecao = Number(state.colecao)
    if (!colecao) return 'Informe o id da coleção.'
    configuracao = { colecao }
  } else {
    if (!state.lista.trim()) return 'Informe o id da lista.'
    configuracao = { lista: state.lista.trim() }
  }
  return {
    nome: state.nome.trim(),
    tipo: state.tipo,
    configuracao,
    ligada: state.ligada,
    monitorar: state.monitorar,
    buscar_ao_adicionar: state.buscarAoAdicionar,
    perfil: Number(state.perfil),
    pasta: state.pasta,
    disponibilidade_minima: state.disponibilidade,
    tags: state.tags,
  }
}

function keySetting(list: ImportList): string {
  const settings = list.settings as Record<string, unknown>
  if (list.kind === 'tmdb_person') return `Pessoa #${settings.pessoa ?? '—'}`
  if (list.kind === 'tmdb_collection') return `Coleção #${settings.colecao ?? '—'}`
  return `Lista #${settings.lista ?? '—'}`
}

function PreviewDialog({ list, onOpenChange }: { list: ImportList; onOpenChange: (open: boolean) => void }) {
  const preview = useQuery({ queryKey: ['lista-previa', list.id], queryFn: () => library.previewList(list.id) })
  const items = preview.data?.filmes ?? []
  return (
    <Dialog open onOpenChange={onOpenChange}>
      <DialogContent className="max-w-2xl">
        <DialogHeader>
          <DialogTitle>Prévia de {list.name}</DialogTitle>
          <DialogDescription>O que essa lista traria numa sincronização agora.</DialogDescription>
        </DialogHeader>
        <div className="max-h-[60vh] overflow-auto">
          {preview.isPending ? (
            <div aria-busy="true" className="grid grid-cols-[repeat(auto-fill,minmax(6.5rem,1fr))] gap-4">
              {Array.from({ length: 8 }, (_, key) => (
                <Skeleton key={key} className="aspect-[2/3] rounded-md" />
              ))}
            </div>
          ) : preview.isError ? (
            <div role="alert" className="flex flex-col items-start gap-3">
              <p className="text-sm text-danger">{preview.error.message}</p>
              <Button onClick={() => void preview.refetch()}>Tentar novamente</Button>
            </div>
          ) : items.length === 0 ? (
            <div className="flex flex-col items-center gap-2 py-10 text-center">
              <SearchX className="size-6 text-content-subtle" aria-hidden="true" />
              <p className="text-sm text-content-muted">Nenhum filme encontrado nessa lista agora.</p>
            </div>
          ) : (
            <ul className="grid grid-cols-[repeat(auto-fill,minmax(6.5rem,1fr))] gap-4">
              {items.map((item) => (
                <li key={item.tmdb}>
                  <div className="relative aspect-[2/3] overflow-hidden rounded-md border border-border bg-surface-raised">
                    {item.poster ? (
                      <img
                        src={item.poster}
                        alt=""
                        loading="lazy"
                        decoding="async"
                        className="size-full object-cover"
                      />
                    ) : (
                      <div className="flex size-full flex-col items-center justify-center gap-2 p-2 text-center">
                        <Film className="size-5 text-content-subtle" aria-hidden="true" />
                      </div>
                    )}
                    {(item.no_catalogo || item.excluido) && (
                      <div className="absolute top-1.5 left-1.5">
                        <Badge tone={item.no_catalogo ? 'success' : 'danger'} className="shadow-sm">
                          {item.no_catalogo ? 'No catálogo' : 'Excluído'}
                        </Badge>
                      </div>
                    )}
                  </div>
                  <p className="mt-1.5 line-clamp-2 text-xs leading-snug">
                    {item.titulo}
                    {item.ano ? ` (${item.ano})` : ''}
                  </p>
                </li>
              ))}
            </ul>
          )}
        </div>
        <DialogFooter>
          <Button variant="ghost" onClick={() => onOpenChange(false)}>
            Fechar
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function EditorDialog({
  list,
  options,
  onOpenChange,
}: {
  list: ImportList | null
  options: LibraryOptions | undefined
  onOpenChange: (open: boolean) => void
}) {
  const queryClient = useQueryClient()
  const [state, setState] = useState<EditorState>(() => (list ? editorFromList(list) : emptyEditor(options)))
  const [error, setError] = useState<string | null>(null)

  const save = useMutation({
    mutationFn: (input: ImportListInput) => (list ? library.updateList(list.id, input) : library.createList(input)),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ['listas'] })
      toast.success(list ? 'Lista atualizada' : 'Lista criada')
      onOpenChange(false)
    },
    onError: (err: Error) => toast.error(err.message),
  })

  return (
    <Dialog open onOpenChange={onOpenChange}>
      <DialogContent className="max-w-xl">
        <DialogHeader>
          <DialogTitle>{list ? `Editar ${list.name}` : 'Nova lista'}</DialogTitle>
          <DialogDescription>
            Filmes de uma pessoa, coleção ou lista do TMDB entram sozinhos no catálogo a cada 12 horas.
          </DialogDescription>
        </DialogHeader>
        <form
          noValidate
          className="grid gap-4"
          onSubmit={(event) => {
            event.preventDefault()
            const input = buildInput(state)
            if (typeof input === 'string') {
              setError(input)
              return
            }
            setError(null)
            save.mutate(input)
          }}
        >
          <div className="grid gap-2">
            <Label htmlFor="lista-nome">Nome</Label>
            <Input
              id="lista-nome"
              value={state.nome}
              onChange={(event) => setState((prev) => ({ ...prev, nome: event.target.value }))}
              placeholder="Ex.: Christopher Nolan"
            />
          </div>

          <div className="grid gap-2">
            <Label htmlFor="lista-tipo">Tipo</Label>
            <Select
              value={state.tipo}
              onValueChange={(value: ImportListKind) => setState((prev) => ({ ...prev, tipo: value }))}
            >
              <SelectTrigger id="lista-tipo">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {(Object.keys(KIND_LABELS) as ImportListKind[]).map((kind) => (
                  <SelectItem key={kind} value={kind}>
                    {KIND_LABELS[kind]}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>

          {state.tipo === 'tmdb_person' && (
            <div className="grid gap-4 rounded-md border border-border p-4">
              <div className="grid gap-2">
                <Label htmlFor="lista-pessoa">Id da pessoa no TMDB</Label>
                <Input
                  id="lista-pessoa"
                  type="number"
                  value={state.pessoa}
                  onChange={(event) => setState((prev) => ({ ...prev, pessoa: event.target.value }))}
                  placeholder="17276"
                />
                <p className="text-xs text-content-subtle">
                  O número no fim da URL da pessoa, ex.: themoviedb.org/person/17276.
                </p>
              </div>
              <div className="flex items-center justify-between gap-6">
                <p id="lista-elenco-titulo" className="text-sm">
                  Como ator/atriz
                </p>
                <Switch
                  checked={state.elenco}
                  onCheckedChange={(on) => setState((prev) => ({ ...prev, elenco: on }))}
                  aria-labelledby="lista-elenco-titulo"
                />
              </div>
              <div className="grid gap-2">
                <p className="text-sm">Departamentos</p>
                {DEPARTMENTS.map((dept) => (
                  <div key={dept.value} className="flex items-center justify-between gap-6">
                    <p id={`lista-dept-${dept.value}`} className="text-sm text-content-muted">
                      {dept.label}
                    </p>
                    <Switch
                      checked={state.departamentos.includes(dept.value)}
                      onCheckedChange={(on) =>
                        setState((prev) => ({
                          ...prev,
                          departamentos: on
                            ? [...prev.departamentos, dept.value]
                            : prev.departamentos.filter((d) => d !== dept.value),
                        }))
                      }
                      aria-labelledby={`lista-dept-${dept.value}`}
                    />
                  </div>
                ))}
              </div>
            </div>
          )}

          {state.tipo === 'tmdb_collection' && (
            <div className="grid gap-2 rounded-md border border-border p-4">
              <Label htmlFor="lista-colecao">Id da coleção</Label>
              <Input
                id="lista-colecao"
                type="number"
                value={state.colecao}
                onChange={(event) => setState((prev) => ({ ...prev, colecao: event.target.value }))}
                placeholder="10"
              />
              <p className="text-xs text-content-subtle">
                O número no fim da URL da coleção, ex.: themoviedb.org/collection/10.
              </p>
            </div>
          )}

          {state.tipo === 'tmdb_list' && (
            <div className="grid gap-2 rounded-md border border-border p-4">
              <Label htmlFor="lista-lista">Id da lista</Label>
              <Input
                id="lista-lista"
                value={state.lista}
                onChange={(event) => setState((prev) => ({ ...prev, lista: event.target.value }))}
                placeholder="8231596"
              />
              <p className="text-xs text-content-subtle">
                O número no fim da URL da lista, ex.: themoviedb.org/list/8231596.
              </p>
            </div>
          )}

          <div className="grid gap-2">
            <Label htmlFor="lista-perfil">Perfil</Label>
            <Select value={state.perfil} onValueChange={(value) => setState((prev) => ({ ...prev, perfil: value }))}>
              <SelectTrigger id="lista-perfil">
                <SelectValue placeholder="Escolha um perfil" />
              </SelectTrigger>
              <SelectContent>
                {(options?.perfis ?? []).map((perfil) => (
                  <SelectItem key={perfil.id} value={String(perfil.id)}>
                    {perfil.nome}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>

          <div className="grid gap-2">
            <Label htmlFor="lista-pasta">Pasta</Label>
            <Select value={state.pasta} onValueChange={(value) => setState((prev) => ({ ...prev, pasta: value }))}>
              <SelectTrigger id="lista-pasta">
                <SelectValue placeholder="Escolha uma pasta" />
              </SelectTrigger>
              <SelectContent>
                {(options?.pastas ?? []).map((pasta) => (
                  <SelectItem key={pasta.caminho} value={pasta.caminho}>
                    {pasta.caminho}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>

          <div className="grid gap-2">
            <Label htmlFor="lista-disponibilidade">Disponibilidade mínima</Label>
            <Select
              value={state.disponibilidade}
              onValueChange={(value) => setState((prev) => ({ ...prev, disponibilidade: value }))}
            >
              <SelectTrigger id="lista-disponibilidade">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {AVAILABILITY.map((item) => (
                  <SelectItem key={item.value} value={item.value}>
                    {item.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>

          <div className="grid gap-3 border-t border-border pt-4">
            <div className="flex items-center justify-between gap-6">
              <p id="lista-ligada-titulo" className="text-sm">
                Ligada
              </p>
              <Switch
                checked={state.ligada}
                onCheckedChange={(on) => setState((prev) => ({ ...prev, ligada: on }))}
                aria-labelledby="lista-ligada-titulo"
              />
            </div>
            <div className="flex items-center justify-between gap-6">
              <p id="lista-monitorar-titulo" className="text-sm">
                Monitorar
              </p>
              <Switch
                checked={state.monitorar}
                onCheckedChange={(on) => setState((prev) => ({ ...prev, monitorar: on }))}
                aria-labelledby="lista-monitorar-titulo"
              />
            </div>
            <div className="flex items-center justify-between gap-6">
              <p id="lista-buscar-titulo" className="text-sm">
                Buscar ao adicionar
              </p>
              <Switch
                checked={state.buscarAoAdicionar}
                onCheckedChange={(on) => setState((prev) => ({ ...prev, buscarAoAdicionar: on }))}
                aria-labelledby="lista-buscar-titulo"
              />
            </div>
          </div>

          {error && (
            <p role="alert" className="text-sm text-danger">
              {error}
            </p>
          )}

          <DialogFooter>
            <Button type="button" variant="ghost" onClick={() => onOpenChange(false)}>
              Cancelar
            </Button>
            <Button type="submit" variant="primary" loading={save.isPending}>
              Salvar
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}

function ListRow({
  list,
  profileName,
  onPreview,
  onEdit,
}: {
  list: ImportList
  profileName: string | undefined
  onPreview: () => void
  onEdit: () => void
}) {
  const queryClient = useQueryClient()

  const sync = useMutation({
    mutationFn: () => library.syncList(list.id),
    onSuccess: (report) => {
      void queryClient.invalidateQueries({ queryKey: ['listas'] })
      const summary = `${report.adicionados.length} adicionados, ${report.ja_no_catalogo} já no catálogo, ${report.excluidos} excluídos`
      if (report.falhas.length > 0) toast.error(`${summary} — ${report.falhas.length} falhas`)
      else toast.success(summary)
    },
    onError: (err: Error) => toast.error(err.message),
  })

  const remove = useMutation({
    mutationFn: () => library.deleteList(list.id),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ['listas'] })
      toast.success('Lista excluída')
    },
    onError: (err: Error) => toast.error(err.message),
  })

  return (
    <li className="rounded-md border border-border p-4">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="min-w-0">
          <p className="font-medium">{list.name}</p>
          <p className="text-xs text-content-subtle">
            {KIND_LABELS[list.kind]} · {keySetting(list)}
          </p>
          <p className="mt-1 text-xs text-content-subtle">
            {profileName ?? 'Perfil removido'} · {list.root_folder}
          </p>
          <div className="mt-2 flex flex-wrap gap-1.5">
            <Badge tone={list.enabled ? 'success' : undefined}>{list.enabled ? 'Ligada' : 'Desligada'}</Badge>
            {list.monitor && <Badge tone="accent">Monitorar</Badge>}
            {list.search_on_add && <Badge tone="accent">Buscar ao adicionar</Badge>}
          </div>
          <p className="mt-2 text-xs text-content-subtle">Sincronizada {formatAgo(list.last_sync)}</p>
          {list.last_error && <p className="mt-1 text-xs text-danger">{list.last_error}</p>}
        </div>
        <div className="flex shrink-0 flex-wrap gap-1.5">
          <Button variant="ghost" size="sm" onClick={onPreview}>
            Prévia
          </Button>
          <Button variant="ghost" size="sm" loading={sync.isPending} onClick={() => sync.mutate()}>
            {!sync.isPending && <RefreshCw aria-hidden="true" />}
            Sincronizar agora
          </Button>
          <Button variant="ghost" size="icon-sm" aria-label="Editar" onClick={onEdit}>
            <Pencil className="size-4" aria-hidden="true" />
          </Button>
          <ConfirmButton
            variant="ghost"
            size="icon-sm"
            aria-label="Excluir"
            loading={remove.isPending}
            title={`Excluir a lista “${list.name}”?`}
            description="Os filmes que ela já trouxe ficam na biblioteca."
            onConfirm={() => remove.mutate()}
          >
            {!remove.isPending && <Trash2 className="size-4" aria-hidden="true" />}
          </ConfirmButton>
        </div>
      </div>
    </li>
  )
}

export function ListsSection() {
  const lists = useQuery({ queryKey: ['listas'], queryFn: library.lists })
  const options = useQuery({ queryKey: ['biblioteca-opcoes'], queryFn: library.options })
  const [previewing, setPreviewing] = useState<ImportList | null>(null)
  const [editing, setEditing] = useState<ImportList | null>(null)
  const [creating, setCreating] = useState(false)

  const list = lists.data?.listas ?? []

  return (
    <section aria-labelledby="listas-titulo" className="max-w-2xl rounded-lg border border-border bg-surface p-6">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h2 id="listas-titulo" className="text-lg font-semibold">
            Listas de importação
          </h2>
          <p className="mt-1 max-w-[60ch] text-sm text-content-muted">
            Filmes de uma pessoa, coleção ou lista do TMDB entram sozinhos no catálogo a cada 12 horas. Filmes excluídos
            nunca voltam.
          </p>
        </div>
        {list.length > 0 && (
          <Button variant="secondary" size="sm" onClick={() => setCreating(true)}>
            <ListPlus aria-hidden="true" />
            Nova lista
          </Button>
        )}
      </div>

      {lists.isPending ? (
        <div aria-busy="true" className="mt-6 grid gap-3">
          {Array.from({ length: 3 }, (_, key) => (
            <Skeleton key={key} className="h-28 w-full" />
          ))}
        </div>
      ) : lists.isError ? (
        <div role="alert" className="mt-6 flex flex-col items-start gap-3">
          <p className="text-sm text-danger">{lists.error.message}</p>
          <Button onClick={() => void lists.refetch()}>Tentar novamente</Button>
        </div>
      ) : list.length === 0 ? (
        <div className="mt-6 flex flex-col items-center gap-3 rounded-md border border-dashed border-border-strong px-6 py-12 text-center">
          <ListPlus className="size-6 text-content-subtle" aria-hidden="true" />
          <p className="max-w-[45ch] text-sm text-content-muted">
            Nenhuma lista ainda. Crie uma para trazer filmes de uma pessoa, coleção ou lista do TMDB sozinho.
          </p>
          <Button variant="primary" onClick={() => setCreating(true)}>
            Nova lista
          </Button>
        </div>
      ) : (
        <ul className={cn('mt-6 grid gap-3')}>
          {list.map((item) => (
            <ListRow
              key={item.id}
              list={item}
              profileName={options.data?.perfis.find((p) => p.id === item.quality_profile_id)?.nome}
              onPreview={() => setPreviewing(item)}
              onEdit={() => setEditing(item)}
            />
          ))}
        </ul>
      )}

      {previewing && <PreviewDialog list={previewing} onOpenChange={(open) => !open && setPreviewing(null)} />}
      {editing && (
        <EditorDialog list={editing} options={options.data} onOpenChange={(open) => !open && setEditing(null)} />
      )}
      {creating && (
        <EditorDialog list={null} options={options.data} onOpenChange={(open) => !open && setCreating(false)} />
      )}
    </section>
  )
}
