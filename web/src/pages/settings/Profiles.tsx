import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { ArrowDown, ArrowUp, Layers, Pencil, Plus, Trash2 } from 'lucide-react'
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
import { Badge, Skeleton, Switch, Tooltip } from '@/components/ui/misc'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import {
  type CustomFormat,
  type LibraryOptions,
  type Profile,
  type ProfileInput,
  type ProfileItem,
  library,
} from '@/lib/api'
import { formatCount } from '@/lib/format'

const LANGUAGES = [
  'Any',
  'Original',
  'Portuguese (Brazil)',
  'Portuguese',
  'English',
  'Spanish',
  'French',
  'Japanese',
  'Korean',
]

function languageLabel(value: string | null): string {
  const v = value ?? 'Any'
  if (v === 'Any') return 'Qualquer idioma'
  if (v === 'Original') return 'Idioma original'
  return v
}

function moveItem(itens: ProfileItem[], index: number, direction: 1 | -1): ProfileItem[] {
  const target = index + direction
  if (target < 0 || target >= itens.length) return itens
  const copy = [...itens]
  const a = copy[index]
  const b = copy[target]
  if (!a || !b) return itens
  copy[index] = b
  copy[target] = a
  return copy
}

/** Ordem de estoque: `itens` guarda pior→melhor; a lista mostra melhor primeiro. */
function displayOrder(itens: ProfileItem[]): { item: ProfileItem; index: number }[] {
  return itens.map((item, index) => ({ item, index })).reverse()
}

interface Draft {
  nome: string
  upgrade: boolean
  cutoffName: string | null
  idioma: string
  itens: ProfileItem[]
  notaMinima: number
  notaCorte: number
  notas: Record<string, number>
}

function draftFromProfile(profile: Profile | null, options: LibraryOptions): Draft {
  const itens = profile
    ? profile.itens
    : options.qualidades.map((q) => ({ nome: q.nome, qualidades: [q.id], permitido: false }))
  const cutoffName = profile?.corte != null ? (profile.itens[profile.corte]?.nome ?? null) : null
  return {
    nome: profile?.nome ?? '',
    upgrade: profile?.upgrade ?? false,
    cutoffName,
    idioma: profile?.idioma ?? 'Any',
    itens,
    notaMinima: profile?.nota_minima ?? 0,
    notaCorte: profile?.nota_corte ?? 0,
    notas: profile?.notas ?? {},
  }
}

function draftToInput(draft: Draft): ProfileInput {
  const corte = draft.upgrade && draft.cutoffName ? draft.itens.findIndex((i) => i.nome === draft.cutoffName) : null
  const notas: Record<string, number> = {}
  for (const [id, value] of Object.entries(draft.notas)) if (value !== 0) notas[id] = value
  return {
    nome: draft.nome.trim(),
    upgrade: draft.upgrade,
    corte: corte != null && corte >= 0 ? corte : null,
    idioma: draft.idioma,
    itens: draft.itens,
    nota_minima: draft.notaMinima,
    nota_corte: draft.notaCorte,
    notas,
  }
}

function ProfileEditor({
  profile,
  options,
  formats,
  editable,
  onClose,
}: {
  profile: Profile | null
  options: LibraryOptions
  formats: CustomFormat[]
  editable: boolean
  onClose: () => void
}) {
  const queryClient = useQueryClient()
  const [draft, setDraft] = useState<Draft>(() => draftFromProfile(profile, options))
  const [errors, setErrors] = useState<{ nome?: string; itens?: string; cutoff?: string }>({})
  const [serverError, setServerError] = useState<string | null>(null)

  const qualityName = (id: number) => options.qualidades.find((q) => q.id === id)?.nome ?? `#${id}`

  const save = useMutation({
    mutationFn: (input: ProfileInput) =>
      profile ? library.updateProfile(profile.id, input) : library.createProfile(input),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ['perfis'] })
      toast.success(profile ? 'Perfil atualizado' : 'Perfil criado')
      onClose()
    },
    onError: (error: Error) => setServerError(error.message),
  })

  function validate(): boolean {
    const next: typeof errors = {}
    if (!draft.nome.trim()) next.nome = 'Dê um nome ao perfil.'
    if (!draft.itens.some((i) => i.permitido)) next.itens = 'Aceite pelo menos uma qualidade.'
    if (draft.upgrade) {
      const cutoff = draft.itens.find((i) => i.nome === draft.cutoffName)
      if (!draft.cutoffName || !cutoff?.permitido) next.cutoff = 'O upgrade vai até uma qualidade aceita.'
    }
    setErrors(next)
    return Object.keys(next).length === 0
  }

  const allowedNames = draft.itens.filter((i) => i.permitido).map((i) => i.nome)

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-w-2xl gap-5">
        <DialogHeader>
          <DialogTitle>{profile ? `Editar ${profile.nome}` : 'Novo perfil'}</DialogTitle>
          <DialogDescription>
            Define o que é aceito, até onde o acervo-hub faz upgrade e como os formatos personalizados pontuam.
          </DialogDescription>
        </DialogHeader>

        <form
          noValidate
          className="grid gap-5"
          onSubmit={(event) => {
            event.preventDefault()
            if (validate()) save.mutate(draftToInput(draft))
          }}
        >
          <div className="grid gap-2">
            <Label htmlFor="perfil-nome">Nome</Label>
            <Input
              id="perfil-nome"
              value={draft.nome}
              onChange={(event) => setDraft((d) => ({ ...d, nome: event.target.value }))}
              aria-invalid={errors.nome ? true : undefined}
              aria-describedby={errors.nome ? 'perfil-nome-erro' : undefined}
              disabled={!editable}
            />
            {errors.nome && (
              <p id="perfil-nome-erro" role="alert" className="text-sm text-danger">
                {errors.nome}
              </p>
            )}
          </div>

          <div className="grid gap-2">
            <Label htmlFor="perfil-idioma">Idioma</Label>
            <Select
              value={draft.idioma}
              onValueChange={(value) => setDraft((d) => ({ ...d, idioma: value }))}
              disabled={!editable}
            >
              <SelectTrigger id="perfil-idioma">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {LANGUAGES.map((value) => (
                  <SelectItem key={value} value={value}>
                    {languageLabel(value)}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>

          <div className="flex items-center justify-between gap-6 border-t border-border pt-4">
            <div>
              <p id="perfil-upgrade-titulo" className="text-sm font-medium">
                Permitir upgrade
              </p>
              <p className="text-xs text-content-subtle">Troca por uma versão melhor até o corte, e para aí.</p>
            </div>
            <Switch
              checked={draft.upgrade}
              onCheckedChange={(on) => setDraft((d) => ({ ...d, upgrade: on }))}
              aria-labelledby="perfil-upgrade-titulo"
              disabled={!editable}
            />
          </div>

          {draft.upgrade && (
            <div className="grid gap-2">
              <Label htmlFor="perfil-corte">Upgrade até</Label>
              <Select
                value={draft.cutoffName ?? undefined}
                onValueChange={(value) => setDraft((d) => ({ ...d, cutoffName: value }))}
                disabled={!editable || allowedNames.length === 0}
              >
                <SelectTrigger id="perfil-corte" aria-invalid={errors.cutoff ? true : undefined}>
                  <SelectValue placeholder={allowedNames.length === 0 ? 'Nenhuma qualidade aceita ainda' : 'Escolha'} />
                </SelectTrigger>
                <SelectContent>
                  {allowedNames.map((name) => (
                    <SelectItem key={name} value={name}>
                      {name}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              {errors.cutoff && (
                <p role="alert" className="text-sm text-danger">
                  {errors.cutoff}
                </p>
              )}
            </div>
          )}

          <div className="grid gap-2 border-t border-border pt-4">
            <p className="text-sm font-medium">Qualidades aceitas</p>
            <p className="text-xs text-content-subtle">
              Da melhor para a pior. Suba ou desça para mudar a preferência.
            </p>
            <ul className="grid gap-1 rounded-md border border-border p-1">
              {displayOrder(draft.itens).map(({ item, index }) => (
                <li key={item.nome} className="flex items-center gap-2 rounded-sm px-2 py-1.5 hover:bg-surface-raised">
                  <Switch
                    checked={item.permitido}
                    onCheckedChange={(on) =>
                      setDraft((d) => ({
                        ...d,
                        itens: d.itens.map((i, idx) => (idx === index ? { ...i, permitido: on } : i)),
                      }))
                    }
                    aria-label={`Aceitar ${item.nome}`}
                    disabled={!editable}
                  />
                  <div className="min-w-0 flex-1">
                    <p className="truncate text-sm">{item.nome}</p>
                    {item.qualidades.length > 1 && (
                      <p className="truncate text-xs text-content-subtle">
                        {item.qualidades.map(qualityName).join(', ')}
                      </p>
                    )}
                  </div>
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon-sm"
                    aria-label="Subir"
                    disabled={!editable || index === draft.itens.length - 1}
                    onClick={() => setDraft((d) => ({ ...d, itens: moveItem(d.itens, index, 1) }))}
                  >
                    <ArrowUp className="size-4" aria-hidden="true" />
                  </Button>
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon-sm"
                    aria-label="Descer"
                    disabled={!editable || index === 0}
                    onClick={() => setDraft((d) => ({ ...d, itens: moveItem(d.itens, index, -1) }))}
                  >
                    <ArrowDown className="size-4" aria-hidden="true" />
                  </Button>
                </li>
              ))}
            </ul>
            {errors.itens && (
              <p role="alert" className="text-sm text-danger">
                {errors.itens}
              </p>
            )}
          </div>

          <div className="grid gap-4 border-t border-border pt-4">
            <p className="text-sm font-medium">Notas</p>
            <div className="grid grid-cols-2 gap-4">
              <div className="grid gap-2">
                <Label htmlFor="perfil-nota-minima">Nota mínima</Label>
                <Input
                  id="perfil-nota-minima"
                  type="number"
                  className="tabular-nums"
                  value={draft.notaMinima}
                  onChange={(event) => setDraft((d) => ({ ...d, notaMinima: Number(event.target.value) || 0 }))}
                  disabled={!editable}
                />
              </div>
              <div className="grid gap-2">
                <Label htmlFor="perfil-nota-corte">Nota de corte do upgrade</Label>
                <Input
                  id="perfil-nota-corte"
                  type="number"
                  className="tabular-nums"
                  value={draft.notaCorte}
                  onChange={(event) => setDraft((d) => ({ ...d, notaCorte: Number(event.target.value) || 0 }))}
                  disabled={!editable}
                />
              </div>
            </div>
            {formats.length > 0 && (
              <div className="grid gap-1">
                <p className="text-xs text-content-subtle">Pontos por formato personalizado</p>
                <ul className="grid gap-1 rounded-md border border-border p-1">
                  {formats.map((format) => (
                    <li key={format.id} className="flex items-center justify-between gap-3 px-2 py-1">
                      <Label htmlFor={`perfil-nota-${format.id}`} className="truncate text-sm font-normal">
                        {format.nome}
                      </Label>
                      <Input
                        id={`perfil-nota-${format.id}`}
                        type="number"
                        className="w-24 tabular-nums"
                        value={draft.notas[String(format.id)] ?? 0}
                        onChange={(event) =>
                          setDraft((d) => ({
                            ...d,
                            notas: { ...d.notas, [String(format.id)]: Number(event.target.value) || 0 },
                          }))
                        }
                        disabled={!editable}
                      />
                    </li>
                  ))}
                </ul>
              </div>
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

function ProfileEditorGate({
  profile,
  editable,
  onClose,
}: {
  profile: Profile | null | 'new'
  editable: boolean
  onClose: () => void
}) {
  const options = useQuery({ queryKey: ['biblioteca-opcoes'], queryFn: library.options })
  const formats = useQuery({ queryKey: ['formatos'], queryFn: library.formats })
  const ready = options.data && formats.data

  if (!ready) {
    return (
      <Dialog open onOpenChange={(open) => !open && onClose()}>
        <DialogContent className="max-w-2xl">
          <DialogHeader>
            <DialogTitle>{profile === 'new' ? 'Novo perfil' : `Editar ${profile?.nome ?? ''}`}</DialogTitle>
          </DialogHeader>
          {options.isError || formats.isError ? (
            <div role="alert" className="grid gap-3">
              <p className="text-sm text-danger">
                {options.error?.message ?? formats.error?.message ?? 'Não foi possível carregar os dados do editor.'}
              </p>
              <Button
                onClick={() => {
                  void options.refetch()
                  void formats.refetch()
                }}
              >
                Tentar novamente
              </Button>
            </div>
          ) : (
            <div aria-busy="true" className="grid gap-3">
              <Skeleton className="h-9 w-full" />
              <Skeleton className="h-9 w-full" />
              <Skeleton className="h-40 w-full" />
            </div>
          )}
        </DialogContent>
      </Dialog>
    )
  }

  return (
    <ProfileEditor
      profile={profile === 'new' ? null : profile}
      options={options.data}
      formats={formats.data.formatos}
      editable={editable}
      onClose={onClose}
    />
  )
}

function ProfileCard({ profile, editable, onEdit }: { profile: Profile; editable: boolean; onEdit: () => void }) {
  const queryClient = useQueryClient()
  const remove = useMutation({
    mutationFn: () => library.deleteProfile(profile.id),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ['perfis'] })
      toast.success('Perfil excluído')
    },
    onError: (error: Error) => toast.error(error.message),
  })

  const accepted = profile.itens.filter((i) => i.permitido).length
  const cutoff = profile.upgrade && profile.corte != null ? profile.itens[profile.corte]?.nome : null

  const deleteButton = (
    <ConfirmButton
      variant="ghost"
      size="sm"
      disabled={profile.em_uso > 0}
      title={`Excluir o perfil “${profile.nome}”?`}
      onConfirm={() => remove.mutate()}
      loading={remove.isPending}
    >
      {!remove.isPending && <Trash2 aria-hidden="true" />}
      Excluir
    </ConfirmButton>
  )

  return (
    <li className="flex flex-wrap items-center justify-between gap-3 rounded-md border border-border p-4">
      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-center gap-2">
          <p className="truncate font-medium">{profile.nome}</p>
          {profile.do_radarr && <Badge>do Radarr</Badge>}
        </div>
        <p className="mt-1 text-sm text-content-muted">
          {cutoff ? `Upgrade até ${cutoff}` : 'Sem upgrade'} · {languageLabel(profile.idioma)} · {formatCount(accepted)}{' '}
          qualidades aceitas
          {profile.em_uso > 0 && ` · usado por ${formatCount(profile.em_uso)} filmes`}
        </p>
      </div>
      <div className="flex shrink-0 gap-2">
        <Button variant="ghost" size="sm" onClick={onEdit}>
          {editable ? <Pencil aria-hidden="true" /> : null}
          {editable ? 'Editar' : 'Ver detalhes'}
        </Button>
        {editable &&
          (profile.em_uso > 0 ? (
            <Tooltip content={`Usado por ${formatCount(profile.em_uso)} filmes`}>
              <span tabIndex={0}>{deleteButton}</span>
            </Tooltip>
          ) : (
            deleteButton
          ))}
      </div>
    </li>
  )
}

export function ProfilesSection({ editable }: { editable: boolean }) {
  const profiles = useQuery({ queryKey: ['perfis'], queryFn: library.profiles })
  const [editing, setEditing] = useState<Profile | null | 'new'>(null)

  const list = profiles.data?.perfis ?? []

  return (
    <section aria-labelledby="perfis-titulo" className="max-w-4xl rounded-lg border border-border bg-surface p-6">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h2 id="perfis-titulo" className="flex items-center gap-2 text-lg font-semibold">
            <Layers className="size-4.5 text-content-subtle" aria-hidden="true" />
            Perfis de qualidade
          </h2>
          <p className="mt-1 max-w-[60ch] text-sm text-content-muted">
            O que cada filme aceita, até onde faz upgrade e como os formatos personalizados somam pontos.
          </p>
        </div>
        {editable && (
          <Button variant="primary" size="sm" onClick={() => setEditing('new')}>
            <Plus aria-hidden="true" />
            Novo perfil
          </Button>
        )}
      </div>

      {profiles.isPending ? (
        <div aria-busy="true" className="mt-6 grid gap-3">
          {Array.from({ length: 3 }, (_, key) => (
            <Skeleton key={key} className="h-16 w-full" />
          ))}
        </div>
      ) : profiles.isError ? (
        <div role="alert" className="mt-6 flex flex-col items-start gap-3">
          <p className="text-sm text-danger">{profiles.error.message}</p>
          <Button onClick={() => void profiles.refetch()}>Tentar novamente</Button>
        </div>
      ) : list.length === 0 ? (
        <div className="mt-6 flex flex-col items-center gap-3 rounded-md border border-dashed border-border-strong px-6 py-10 text-center">
          <Layers className="size-6 text-content-subtle" aria-hidden="true" />
          <p className="text-sm text-content-muted">Nenhum perfil ainda.</p>
          {editable && (
            <Button variant="primary" onClick={() => setEditing('new')}>
              <Plus aria-hidden="true" />
              Novo perfil
            </Button>
          )}
        </div>
      ) : (
        <ul className="mt-6 grid gap-3">
          {list.map((profile) => (
            <ProfileCard key={profile.id} profile={profile} editable={editable} onEdit={() => setEditing(profile)} />
          ))}
        </ul>
      )}

      {editing !== null && <ProfileEditorGate profile={editing} editable={editable} onClose={() => setEditing(null)} />}
    </section>
  )
}
