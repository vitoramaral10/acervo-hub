import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { FlaskConical, Pencil, Plus, SlidersHorizontal, Trash2, X } from 'lucide-react'
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
import { type CustomFormat, type CustomFormatInput, type FormatSpec, library } from '@/lib/api'
import { formatCount } from '@/lib/format'

type Tipo = FormatSpec['tipo']

const TYPE_LABELS: Record<Tipo, string> = {
  titulo: 'Título (regex)',
  grupo: 'Grupo (regex)',
  edicao: 'Edição (regex)',
  idioma: 'Idioma',
  fonte: 'Fonte',
  resolucao: 'Resolução',
  modificador: 'Modificador',
  tamanho: 'Tamanho (GB)',
  flag: 'Flag do indexador',
}

const SUMMARY_LABELS: Record<Tipo, string> = {
  titulo: 'Título',
  grupo: 'Grupo',
  edicao: 'Edição',
  idioma: 'Idioma',
  fonte: 'Fonte',
  resolucao: 'Resolução',
  modificador: 'Modificador',
  tamanho: 'Tamanho',
  flag: 'Flag',
}

const REGEX_TYPES: Tipo[] = ['titulo', 'grupo', 'edicao']

const IDIOMAS = ['Original', 'Any', 'Portuguese (Brazil)', 'Portuguese', 'English', 'Spanish', 'French', 'Japanese']

function idiomaLabel(value: string) {
  if (value === 'Any') return 'Qualquer'
  if (value === 'Original') return 'Original'
  return value
}

const FONTES: { value: string; label: string }[] = [
  { value: 'cam', label: 'CAM' },
  { value: 'telesync', label: 'Telesync' },
  { value: 'telecine', label: 'Telecine' },
  { value: 'workprint', label: 'Workprint' },
  { value: 'dvd', label: 'DVD' },
  { value: 'tv', label: 'TV' },
  { value: 'webdl', label: 'WEB-DL' },
  { value: 'webrip', label: 'WEBRip' },
  { value: 'bluray', label: 'Blu-ray' },
]

const RESOLUCOES = [480, 576, 720, 1080, 2160]

const MODIFICADORES: { value: string; label: string }[] = [
  { value: 'regional', label: 'Regional' },
  { value: 'screener', label: 'Screener' },
  { value: 'rawhd', label: 'Raw-HD' },
  { value: 'brdisk', label: 'BR-Disk' },
  { value: 'remux', label: 'Remux' },
]

const FLAGS: { value: number; label: string }[] = [
  { value: 1, label: '1 Freeleech' },
  { value: 2, label: '2 Halfleech' },
  { value: 4, label: '4 Upload dobrado' },
  { value: 8, label: '8 Golden' },
  { value: 16, label: '16 Aprovado' },
  { value: 32, label: '32 Internal' },
]

/** Rascunho local de uma especificação: um shape só, convertido para `FormatSpec` ao salvar. */
interface SpecDraft {
  key: string
  nome: string
  tipo: Tipo
  valor: string
  minimo: string
  maximo: string
  negar: boolean
  obrigatoria: boolean
}

let nextKey = 0
function newKey() {
  nextKey += 1
  return `novo-${nextKey}`
}

function draftFromSpec(spec: FormatSpec): SpecDraft {
  const base = {
    key: newKey(),
    nome: spec.nome,
    tipo: spec.tipo,
    negar: spec.negar ?? false,
    obrigatoria: spec.obrigatoria ?? false,
  }
  if (spec.tipo === 'tamanho') return { ...base, valor: '', minimo: String(spec.minimo), maximo: String(spec.maximo) }
  if (spec.tipo === 'resolucao' || spec.tipo === 'flag')
    return { ...base, valor: String(spec.valor), minimo: '', maximo: '' }
  return { ...base, valor: spec.valor, minimo: '', maximo: '' }
}

function blankDraft(): SpecDraft {
  return {
    key: newKey(),
    nome: '',
    tipo: 'titulo',
    valor: '',
    minimo: '',
    maximo: '',
    negar: false,
    obrigatoria: false,
  }
}

function draftToSpec(draft: SpecDraft): FormatSpec {
  const nome = draft.nome.trim()
  const negar = draft.negar || undefined
  const obrigatoria = draft.obrigatoria || undefined
  switch (draft.tipo) {
    case 'titulo':
    case 'grupo':
    case 'edicao':
    case 'idioma':
    case 'fonte':
    case 'modificador':
      return { tipo: draft.tipo, valor: draft.valor, nome, negar, obrigatoria }
    case 'resolucao':
      return { tipo: 'resolucao', valor: Number(draft.valor) || 0, nome, negar, obrigatoria }
    case 'flag':
      return { tipo: 'flag', valor: Number(draft.valor) || 0, nome, negar, obrigatoria }
    case 'tamanho':
      return {
        tipo: 'tamanho',
        minimo: Number(draft.minimo) || 0,
        maximo: Number(draft.maximo) || 0,
        nome,
        negar,
        obrigatoria,
      }
  }
}

function summarizeSpec(spec: FormatSpec): string {
  const label = SUMMARY_LABELS[spec.tipo]
  const negado = spec.negar ? 'não ' : ''
  let value: string
  switch (spec.tipo) {
    case 'titulo':
    case 'grupo':
    case 'edicao':
      value = spec.valor
      break
    case 'idioma':
      value = idiomaLabel(spec.valor)
      break
    case 'fonte':
      value = FONTES.find((f) => f.value === spec.valor)?.label ?? spec.valor
      break
    case 'modificador':
      value = MODIFICADORES.find((m) => m.value === spec.valor)?.label ?? spec.valor
      break
    case 'resolucao':
      value = `${spec.valor}p`
      break
    case 'flag':
      value = FLAGS.find((f) => f.value === spec.valor)?.label ?? String(spec.valor)
      break
    case 'tamanho':
      value = `${spec.minimo}–${spec.maximo} GB`
      break
  }
  const marca = spec.obrigatoria ? '*' : ''
  return `${label}: ${negado}${value}${marca}`
}

function SpecValueField({
  draft,
  editable,
  onChange,
}: {
  draft: SpecDraft
  editable: boolean
  onChange: (draft: SpecDraft) => void
}) {
  const fieldId = `spec-valor-${draft.key}`
  if (REGEX_TYPES.includes(draft.tipo)) {
    return (
      <div className="grid gap-1.5">
        <Label htmlFor={fieldId}>Expressão regular</Label>
        <Input
          id={fieldId}
          className="font-mono"
          value={draft.valor}
          onChange={(event) => onChange({ ...draft, valor: event.target.value })}
          disabled={!editable}
        />
      </div>
    )
  }
  if (draft.tipo === 'idioma') {
    return (
      <div className="grid gap-1.5">
        <Label htmlFor={fieldId}>Idioma</Label>
        <Select
          value={draft.valor || undefined}
          onValueChange={(value) => onChange({ ...draft, valor: value })}
          disabled={!editable}
        >
          <SelectTrigger id={fieldId}>
            <SelectValue placeholder="Escolha" />
          </SelectTrigger>
          <SelectContent>
            {IDIOMAS.map((value) => (
              <SelectItem key={value} value={value}>
                {idiomaLabel(value)}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>
    )
  }
  if (draft.tipo === 'fonte') {
    return (
      <div className="grid gap-1.5">
        <Label htmlFor={fieldId}>Fonte</Label>
        <Select
          value={draft.valor || undefined}
          onValueChange={(value) => onChange({ ...draft, valor: value })}
          disabled={!editable}
        >
          <SelectTrigger id={fieldId}>
            <SelectValue placeholder="Escolha" />
          </SelectTrigger>
          <SelectContent>
            {FONTES.map((f) => (
              <SelectItem key={f.value} value={f.value}>
                {f.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>
    )
  }
  if (draft.tipo === 'resolucao') {
    return (
      <div className="grid gap-1.5">
        <Label htmlFor={fieldId}>Resolução</Label>
        <Select
          value={draft.valor || undefined}
          onValueChange={(value) => onChange({ ...draft, valor: value })}
          disabled={!editable}
        >
          <SelectTrigger id={fieldId}>
            <SelectValue placeholder="Escolha" />
          </SelectTrigger>
          <SelectContent>
            {RESOLUCOES.map((r) => (
              <SelectItem key={r} value={String(r)}>
                {r}p
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>
    )
  }
  if (draft.tipo === 'modificador') {
    return (
      <div className="grid gap-1.5">
        <Label htmlFor={fieldId}>Modificador</Label>
        <Select
          value={draft.valor || undefined}
          onValueChange={(value) => onChange({ ...draft, valor: value })}
          disabled={!editable}
        >
          <SelectTrigger id={fieldId}>
            <SelectValue placeholder="Escolha" />
          </SelectTrigger>
          <SelectContent>
            {MODIFICADORES.map((m) => (
              <SelectItem key={m.value} value={m.value}>
                {m.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>
    )
  }
  if (draft.tipo === 'flag') {
    return (
      <div className="grid gap-1.5">
        <Label htmlFor={fieldId}>Flag</Label>
        <Select
          value={draft.valor || undefined}
          onValueChange={(value) => onChange({ ...draft, valor: value })}
          disabled={!editable}
        >
          <SelectTrigger id={fieldId}>
            <SelectValue placeholder="Escolha" />
          </SelectTrigger>
          <SelectContent>
            {FLAGS.map((f) => (
              <SelectItem key={f.value} value={String(f.value)}>
                {f.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>
    )
  }
  // tamanho
  return (
    <div className="grid grid-cols-2 gap-3">
      <div className="grid gap-1.5">
        <Label htmlFor={`${fieldId}-min`}>Mínimo (GB)</Label>
        <Input
          id={`${fieldId}-min`}
          type="number"
          className="tabular-nums"
          value={draft.minimo}
          onChange={(event) => onChange({ ...draft, minimo: event.target.value })}
          disabled={!editable}
        />
      </div>
      <div className="grid gap-1.5">
        <Label htmlFor={`${fieldId}-max`}>Máximo (GB)</Label>
        <Input
          id={`${fieldId}-max`}
          type="number"
          className="tabular-nums"
          value={draft.maximo}
          onChange={(event) => onChange({ ...draft, maximo: event.target.value })}
          disabled={!editable}
        />
      </div>
    </div>
  )
}

function SpecRow({
  draft,
  editable,
  onChange,
  onRemove,
}: {
  draft: SpecDraft
  editable: boolean
  onChange: (draft: SpecDraft) => void
  onRemove: () => void
}) {
  return (
    <li className="grid gap-3 rounded-md border border-border p-3">
      <div className="flex items-start gap-3">
        <div className="grid flex-1 gap-1.5">
          <Label htmlFor={`spec-nome-${draft.key}`}>Nome</Label>
          <Input
            id={`spec-nome-${draft.key}`}
            value={draft.nome}
            onChange={(event) => onChange({ ...draft, nome: event.target.value })}
            disabled={!editable}
          />
        </div>
        <div className="grid flex-1 gap-1.5">
          <Label htmlFor={`spec-tipo-${draft.key}`}>Tipo</Label>
          <Select
            value={draft.tipo}
            onValueChange={(value) =>
              onChange({ ...blankDraft(), key: draft.key, nome: draft.nome, tipo: value as Tipo })
            }
            disabled={!editable}
          >
            <SelectTrigger id={`spec-tipo-${draft.key}`}>
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {(Object.keys(TYPE_LABELS) as Tipo[]).map((tipo) => (
                <SelectItem key={tipo} value={tipo}>
                  {TYPE_LABELS[tipo]}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
        {editable && (
          <Button
            type="button"
            variant="ghost"
            size="icon-sm"
            aria-label="Remover especificação"
            className="mt-6"
            onClick={onRemove}
          >
            <X className="size-4" aria-hidden="true" />
          </Button>
        )}
      </div>

      <SpecValueField draft={draft} editable={editable} onChange={onChange} />

      <div className="flex flex-wrap gap-6">
        <label className="flex items-center gap-2">
          <Switch
            checked={draft.negar}
            onCheckedChange={(on) => onChange({ ...draft, negar: on })}
            disabled={!editable}
          />
          <span className="text-sm">Negar</span>
        </label>
        <label className="flex items-center gap-2">
          <Switch
            checked={draft.obrigatoria}
            onCheckedChange={(on) => onChange({ ...draft, obrigatoria: on })}
            disabled={!editable}
          />
          <span className="text-sm">Obrigatória</span>
        </label>
      </div>
    </li>
  )
}

function FormatEditor({
  format,
  editable,
  onClose,
}: {
  format: CustomFormat | null
  editable: boolean
  onClose: () => void
}) {
  const queryClient = useQueryClient()
  const [nome, setNome] = useState(format?.nome ?? '')
  const [noNomeDoArquivo, setNoNomeDoArquivo] = useState(format?.no_nome_do_arquivo ?? false)
  const [specs, setSpecs] = useState<SpecDraft[]>(() => (format ? format.especificacoes.map(draftFromSpec) : []))
  const [errors, setErrors] = useState<{ nome?: string; specs?: string }>({})
  const [serverError, setServerError] = useState<string | null>(null)

  const save = useMutation({
    mutationFn: (input: CustomFormatInput) =>
      format ? library.updateFormat(format.id, input) : library.createFormat(input),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ['formatos'] })
      toast.success(format ? 'Formato atualizado' : 'Formato criado')
      onClose()
    },
    onError: (error: Error) => setServerError(error.message),
  })

  function validate(): boolean {
    const next: typeof errors = {}
    if (!nome.trim()) next.nome = 'Dê um nome ao formato.'
    if (specs.length === 0) next.specs = 'Adicione pelo menos uma especificação.'
    if (specs.some((s) => !s.nome.trim())) next.specs = 'Toda especificação precisa de um nome.'
    setErrors(next)
    return Object.keys(next).length === 0
  }

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-w-2xl gap-5">
        <DialogHeader>
          <DialogTitle>{format ? `Editar ${format.nome}` : 'Novo formato personalizado'}</DialogTitle>
          <DialogDescription>
            Dentro de um formato, toda especificação obrigatória precisa casar, e de cada tipo pelo menos uma das
            outras.
          </DialogDescription>
        </DialogHeader>

        <form
          noValidate
          className="grid gap-5"
          onSubmit={(event) => {
            event.preventDefault()
            if (!validate()) return
            save.mutate({
              nome: nome.trim(),
              no_nome_do_arquivo: noNomeDoArquivo,
              especificacoes: specs.map(draftToSpec),
            })
          }}
        >
          <div className="grid gap-2">
            <Label htmlFor="formato-nome">Nome</Label>
            <Input
              id="formato-nome"
              value={nome}
              onChange={(event) => setNome(event.target.value)}
              aria-invalid={errors.nome ? true : undefined}
              aria-describedby={errors.nome ? 'formato-nome-erro' : undefined}
              disabled={!editable}
            />
            {errors.nome && (
              <p id="formato-nome-erro" role="alert" className="text-sm text-danger">
                {errors.nome}
              </p>
            )}
          </div>

          <div className="flex items-center justify-between gap-6 border-t border-border pt-4">
            <div>
              <p id="formato-nome-arquivo-titulo" className="text-sm font-medium">
                Incluir no nome do arquivo
              </p>
              <p className="text-xs text-content-subtle">O nome do formato aparece ao renomear o arquivo.</p>
            </div>
            <Switch
              checked={noNomeDoArquivo}
              onCheckedChange={setNoNomeDoArquivo}
              aria-labelledby="formato-nome-arquivo-titulo"
              disabled={!editable}
            />
          </div>

          <div className="grid gap-3 border-t border-border pt-4">
            <div className="flex items-center justify-between gap-3">
              <p className="text-sm font-medium">Especificações</p>
              {editable && (
                <Button type="button" variant="ghost" size="sm" onClick={() => setSpecs((s) => [...s, blankDraft()])}>
                  <Plus aria-hidden="true" />
                  Adicionar especificação
                </Button>
              )}
            </div>
            {specs.length === 0 ? (
              <p className="text-sm text-content-subtle">Nenhuma especificação ainda.</p>
            ) : (
              <ul className="grid gap-3">
                {specs.map((spec) => (
                  <SpecRow
                    key={spec.key}
                    draft={spec}
                    editable={editable}
                    onChange={(next) => setSpecs((all) => all.map((s) => (s.key === spec.key ? next : s)))}
                    onRemove={() => setSpecs((all) => all.filter((s) => s.key !== spec.key))}
                  />
                ))}
              </ul>
            )}
            {errors.specs && (
              <p role="alert" className="text-sm text-danger">
                {errors.specs}
              </p>
            )}
          </div>

          {serverError && (
            <p role="alert" className="text-sm text-danger">
              {serverError}
            </p>
          )}

          <DialogFooter>
            <Button type="button" variant="ghost" onClick={onClose}>
              {editable ? 'Cancelar' : 'Fechar'}
            </Button>
            {editable && (
              <Button type="submit" variant="primary" loading={save.isPending}>
                Salvar
              </Button>
            )}
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}

function FormatCard({ format, editable, onEdit }: { format: CustomFormat; editable: boolean; onEdit: () => void }) {
  const queryClient = useQueryClient()
  const remove = useMutation({
    mutationFn: () => library.deleteFormat(format.id),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ['formatos'] })
      toast.success('Formato excluído')
    },
    onError: (error: Error) => toast.error(error.message),
  })

  return (
    <li className="flex items-center justify-between gap-3 rounded-md border border-border p-4">
      <div className="min-w-0 flex-1">
        <p className="truncate font-medium">{format.nome}</p>
        <p
          className="mt-1 truncate text-sm text-content-muted"
          title={format.especificacoes.map(summarizeSpec).join(' · ')}
        >
          {formatCount(format.especificacoes.length)}{' '}
          {format.especificacoes.length === 1 ? 'especificação' : 'especificações'}
          {format.especificacoes.length > 0 && ` · ${format.especificacoes.map(summarizeSpec).join(' · ')}`}
        </p>
      </div>
      <div className="flex shrink-0 gap-2">
        <Button variant="ghost" size="sm" onClick={onEdit}>
          {editable ? <Pencil aria-hidden="true" /> : null}
          {editable ? 'Editar' : 'Ver detalhes'}
        </Button>
        {editable && (
          <ConfirmButton
            variant="ghost"
            size="sm"
            loading={remove.isPending}
            title={`Excluir o formato “${format.nome}”?`}
            description="A nota dele sai de todos os perfis."
            onConfirm={() => remove.mutate()}
          >
            {!remove.isPending && <Trash2 aria-hidden="true" />}
            Excluir
          </ConfirmButton>
        )}
      </div>
    </li>
  )
}

function TestBox() {
  const [titulo, setTitulo] = useState('')
  const test = useMutation({ mutationFn: (value: string) => library.testFormats(value) })

  return (
    <div className="rounded-md border border-border bg-surface-raised p-4">
      <p className="flex items-center gap-2 text-sm font-medium">
        <FlaskConical className="size-4 text-content-subtle" aria-hidden="true" />
        Testar um nome de release
      </p>
      <form
        noValidate
        className="mt-3 flex flex-col gap-2 sm:flex-row"
        onSubmit={(event) => {
          event.preventDefault()
          if (titulo.trim()) test.mutate(titulo.trim())
        }}
      >
        <label className="flex-1">
          <span className="sr-only">Nome do release</span>
          <Input
            value={titulo}
            onChange={(event) => setTitulo(event.target.value)}
            placeholder="Filme.2024.1080p.BluRay.x265-GRUPO"
            className="font-mono"
          />
        </label>
        <Button type="submit" loading={test.isPending} disabled={!titulo.trim()}>
          Testar
        </Button>
      </form>

      {test.isError && (
        <p role="alert" className="mt-3 text-sm text-danger">
          {test.error.message}
        </p>
      )}

      {test.data && (
        <dl className="mt-3 grid gap-2 text-sm">
          <div className="flex flex-wrap items-center gap-2">
            <dt className="text-xs text-content-subtle">Qualidade</dt>
            <dd>
              <Badge>{test.data.qualidade}</Badge>
            </dd>
          </div>
          {test.data.idiomas.length > 0 && (
            <div className="flex flex-wrap items-center gap-2">
              <dt className="text-xs text-content-subtle">Idiomas</dt>
              <dd className="flex flex-wrap gap-1">
                {test.data.idiomas.map((idioma) => (
                  <Badge key={idioma}>{idioma}</Badge>
                ))}
              </dd>
            </div>
          )}
          {test.data.grupo && (
            <div className="flex flex-wrap items-center gap-2">
              <dt className="text-xs text-content-subtle">Grupo</dt>
              <dd className="font-mono text-xs">{test.data.grupo}</dd>
            </div>
          )}
          <div className="flex flex-wrap items-center gap-2">
            <dt className="text-xs text-content-subtle">Formatos que casam</dt>
            <dd className="flex flex-wrap gap-1">
              {test.data.formatos.length === 0 ? (
                <span className="text-content-subtle">nenhum</span>
              ) : (
                test.data.formatos.map((f) => (
                  <Badge key={f.id} tone="accent">
                    {f.nome}
                  </Badge>
                ))
              )}
            </dd>
          </div>
        </dl>
      )}
    </div>
  )
}

export function FormatsSection({ editable }: { editable: boolean }) {
  const formats = useQuery({ queryKey: ['formatos'], queryFn: library.formats })
  const [editing, setEditing] = useState<CustomFormat | null | 'new'>(null)

  const list = formats.data?.formatos ?? []

  return (
    <section aria-labelledby="formatos-titulo" className="max-w-4xl rounded-lg border border-border bg-surface p-6">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h2 id="formatos-titulo" className="flex items-center gap-2 text-lg font-semibold">
            <SlidersHorizontal className="size-4.5 text-content-subtle" aria-hidden="true" />
            Formatos personalizados
          </h2>
          <p className="mt-1 max-w-[60ch] text-sm text-content-muted">
            Padrões que reconhecem grupo, edição, fonte ou outra marca no nome do release, para os perfis pontuarem.
          </p>
        </div>
        {editable && (
          <Button variant="primary" size="sm" onClick={() => setEditing('new')}>
            <Plus aria-hidden="true" />
            Novo formato
          </Button>
        )}
      </div>

      <div className="mt-6">
        <TestBox />
      </div>

      {formats.isPending ? (
        <div aria-busy="true" className="mt-6 grid gap-3">
          {Array.from({ length: 3 }, (_, key) => (
            <Skeleton key={key} className="h-16 w-full" />
          ))}
        </div>
      ) : formats.isError ? (
        <div role="alert" className="mt-6 flex flex-col items-start gap-3">
          <p className="text-sm text-danger">{formats.error.message}</p>
          <Button onClick={() => void formats.refetch()}>Tentar novamente</Button>
        </div>
      ) : list.length === 0 ? (
        <div className="mt-6 flex flex-col items-center gap-3 rounded-md border border-dashed border-border-strong px-6 py-10 text-center">
          <SlidersHorizontal className="size-6 text-content-subtle" aria-hidden="true" />
          <p className="text-sm text-content-muted">Nenhum formato ainda.</p>
          {editable && (
            <Button variant="primary" onClick={() => setEditing('new')}>
              <Plus aria-hidden="true" />
              Novo formato
            </Button>
          )}
        </div>
      ) : (
        <ul className="mt-6 grid gap-3">
          {list.map((format) => (
            <FormatCard key={format.id} format={format} editable={editable} onEdit={() => setEditing(format)} />
          ))}
        </ul>
      )}

      {editing !== null && (
        <FormatEditor
          format={editing === 'new' ? null : editing}
          editable={editable}
          onClose={() => setEditing(null)}
        />
      )}
    </section>
  )
}
