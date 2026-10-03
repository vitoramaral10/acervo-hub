import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { type ReactNode, useEffect, useState } from 'react'
import { toast } from 'sonner'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Skeleton, Switch } from '@/components/ui/misc'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { type DecisionRules, type RulesView, library } from '@/lib/api'

const PROPERS = [
  { value: 'preferir_e_atualizar', label: 'Preferir e trocar pelo PROPER/REPACK' },
  { value: 'nao_atualizar', label: 'Preferir, mas não trocar o que já tem' },
  { value: 'nao_preferir', label: 'Ignorar PROPER/REPACK' },
] as const

export function Field({ id, label, help, children }: { id: string; label: string; help?: string; children: ReactNode }) {
  return (
    <div className="grid gap-1.5">
      <Label htmlFor={id}>{label}</Label>
      {children}
      {help && (
        <p id={`${id}-ajuda`} className="text-xs text-content-subtle">
          {help}
        </p>
      )}
    </div>
  )
}

export function Toggle({
  id,
  label,
  help,
  checked,
  disabled,
  onChange,
}: {
  id: string
  label: string
  help?: string
  checked: boolean
  disabled?: boolean
  onChange: (on: boolean) => void
}) {
  return (
    <div className="flex items-start justify-between gap-4">
      <div>
        <Label htmlFor={id}>{label}</Label>
        {help && <p className="text-xs text-content-subtle">{help}</p>}
      </div>
      <Switch id={id} checked={checked} disabled={disabled} onCheckedChange={onChange} />
    </div>
  )
}

const number = (value: string) => (value.trim() === '' ? 0 : Math.max(0, Math.trunc(Number(value))))

export function RulesSection({ view }: { view: RulesView }) {
  const queryClient = useQueryClient()
  const [rules, setRules] = useState<DecisionRules>(view.regras)
  useEffect(() => setRules(view.regras), [view.regras])
  const dirty = JSON.stringify(rules) !== JSON.stringify(view.regras)
  const save = useMutation({
    mutationFn: () => library.saveRules(rules),
    onSuccess: () => toast.success('Regras salvas'),
    onError: (error: Error) => toast.error(error.message),
    onSettled: () => void queryClient.invalidateQueries({ queryKey: ['regras'] }),
  })
  // Na tela a folga é em GB; no banco, em MB. O texto fica à parte para não brigar com a digitação.
  const [reserve, setReserve] = useState(String(view.regras.folga_minima_mb / 1024))
  useEffect(() => setReserve(String(view.regras.folga_minima_mb / 1024)), [view.regras.folga_minima_mb])
  const set = <K extends keyof DecisionRules>(key: K, value: DecisionRules[K]) =>
    setRules((current) => ({ ...current, [key]: value }))
  const indexers = Array.from(new Set([...view.indexadores, ...Object.keys(rules.indexadores)])).sort()

  return (
    <section aria-labelledby="regras-titulo" className="rounded-lg border border-border bg-surface p-6">
      <h2 id="regras-titulo" className="text-lg font-semibold">
        Regras de decisão
      </h2>
      <p className="mt-1 max-w-[65ch] text-sm text-content-muted">O que vale para todo release, em todo perfil.</p>
      <form
        className="mt-6 grid gap-8"
        onSubmit={(event) => {
          event.preventDefault()
          save.mutate()
        }}
      >
        <fieldset className="grid gap-4 sm:grid-cols-2">
          <legend className="mb-2 text-sm font-semibold">Tamanho</legend>
          <Field id="teto" label="Tamanho máximo (MB)" help="Zero é sem teto.">
            <Input
              id="teto"
              type="number"
              min={0}
              value={rules.tamanho_maximo_mb}
              onChange={(e) => set('tamanho_maximo_mb', number(e.target.value))}
              aria-describedby="teto-ajuda"
            />
          </Field>
        </fieldset>

        <fieldset className="grid gap-4 sm:grid-cols-2">
          <legend className="mb-2 text-sm font-semibold">Releases</legend>
          <Field id="propers" label="PROPER e REPACK">
            <Select
              value={rules.propers}
              onValueChange={(value) => set('propers', value as DecisionRules['propers'])}
            >
              <SelectTrigger id="propers">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {PROPERS.map((p) => (
                  <SelectItem key={p.value} value={p.value}>
                    {p.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </Field>
          <Field id="carencia" label="Carência após a disponibilidade (dias)">
            <Input
              id="carencia"
              type="number"
              min={0}
              value={rules.carencia_dias}
              onChange={(e) => set('carencia_dias', number(e.target.value))}
            />
          </Field>
          <Toggle
            id="legenda"
            label="Aceitar legenda embutida"
            checked={rules.aceitar_legenda_embutida}
            onChange={(on) => set('aceitar_legenda_embutida', on)}
          />
          <Toggle
            id="flags"
            label="Preferir flags do indexador"
            help="Freeleech, internal e afins pesam na escolha."
            checked={rules.preferir_flags_do_indexador}
            onChange={(on) => set('preferir_flags_do_indexador', on)}
          />
          <div className="sm:col-span-2">
            <Field
              id="liberadas"
              label="Legendas embutidas liberadas"
              help="Termos separados por vírgula que liberam a legenda embutida (ex.: PT-BR)."
            >
              <Input
                id="liberadas"
                value={rules.legendas_embutidas_liberadas}
                onChange={(e) => set('legendas_embutidas_liberadas', e.target.value)}
                aria-describedby="liberadas-ajuda"
              />
            </Field>
          </div>
        </fieldset>

        <fieldset className="grid gap-4 sm:grid-cols-2">
          <legend className="mb-2 text-sm font-semibold">Espera</legend>
          <Field
            id="atraso"
            label="Esperar antes de pegar (minutos)"
            help="Só a busca automática espera; dá tempo de sair uma versão melhor."
          >
            <Input
              id="atraso"
              type="number"
              min={0}
              value={rules.atraso.minutos}
              onChange={(e) => set('atraso', { ...rules.atraso, minutos: number(e.target.value) })}
              aria-describedby="atraso-ajuda"
            />
          </Field>
          <div className="sm:col-span-2">
            <Toggle
              id="atraso-melhor"
              label="Não esperar se já for a melhor qualidade do perfil"
              checked={rules.atraso.pular_se_melhor_qualidade}
              onChange={(on) => set('atraso', { ...rules.atraso, pular_se_melhor_qualidade: on })}
            />
          </div>
        </fieldset>

        <fieldset className="grid gap-4 sm:grid-cols-2">
          <legend className="mb-2 text-sm font-semibold">Disco</legend>
          <Field
            id="folga"
            label="Espaço livre reservado"
            help="A fila de downloads não inicia torrent que deixaria o disco com menos que isso."
          >
            <div className="flex items-center gap-2">
              <Input
                id="folga"
                type="number"
                min={0}
                step={0.5}
                value={reserve}
                onChange={(e) => {
                  setReserve(e.target.value)
                  const gb = Number(e.target.value)
                  if (e.target.value.trim() !== '' && Number.isFinite(gb) && gb >= 0) {
                    set('folga_minima_mb', Math.round(gb * 1024))
                  }
                }}
                aria-describedby="folga-ajuda"
                className="max-w-32"
              />
              <span className="text-sm text-content-muted">GB</span>
            </div>
          </Field>
          <Field
            id="simultaneos"
            label="Downloads simultâneos"
            help="Quantos torrents do acervo baixam ao mesmo tempo; os demais esperam na fila. Mínimo 1."
          >
            <Input
              id="simultaneos"
              type="number"
              min={1}
              step={1}
              value={rules.downloads_simultaneos}
              onChange={(e) => set('downloads_simultaneos', Math.max(1, number(e.target.value)))}
              aria-describedby="simultaneos-ajuda"
              className="max-w-32"
            />
          </Field>
        </fieldset>

        {indexers.length > 0 && (
          <fieldset>
            <legend className="mb-2 text-sm font-semibold">Indexadores</legend>
            <p className="mb-3 text-xs text-content-subtle">
              Prioridade menor ganha no empate de qualidade; release com menos seeders que o mínimo é recusado.
            </p>
            <table className="w-full text-sm">
              <thead className="text-left text-xs text-content-subtle">
                <tr>
                  <th scope="col" className="py-2 font-medium">
                    Indexador
                  </th>
                  <th scope="col" className="w-32 py-2 font-medium">
                    Prioridade
                  </th>
                  <th scope="col" className="w-32 py-2 font-medium">
                    Seeders mínimos
                  </th>
                </tr>
              </thead>
              <tbody className="divide-y divide-border">
                {indexers.map((name) => {
                  const current = rules.indexadores[name] ?? { prioridade: 25, seeders_minimos: 1 }
                  const update = (patch: Partial<typeof current>) =>
                    set('indexadores', { ...rules.indexadores, [name]: { ...current, ...patch } })
                  return (
                    <tr key={name}>
                      <td className="py-2">{name}</td>
                      <td className="py-2 pr-3">
                        <Input
                          type="number"
                          min={1}
                          aria-label={`Prioridade de ${name}`}
                          value={current.prioridade}
                          onChange={(e) => update({ prioridade: Math.max(1, number(e.target.value)) })}
                        />
                      </td>
                      <td className="py-2">
                        <Input
                          type="number"
                          min={0}
                          aria-label={`Seeders mínimos de ${name}`}
                          value={current.seeders_minimos}
                          onChange={(e) => update({ seeders_minimos: number(e.target.value) })}
                        />
                      </td>
                    </tr>
                  )
                })}
              </tbody>
            </table>
          </fieldset>
        )}

        <div className="flex gap-2">
          <Button type="submit" variant="primary" disabled={!dirty} loading={save.isPending}>
            Salvar regras
          </Button>
          {dirty && (
            <Button type="button" variant="ghost" onClick={() => setRules(view.regras)}>
              Descartar
            </Button>
          )}
        </div>
      </form>
    </section>
  )
}

/** Carrega as regras para o formulário. */
export function useRules() {
  return useQuery({ queryKey: ['regras'], queryFn: library.rules })
}

export function RulesSkeleton() {
  return <Skeleton className="h-64 rounded-lg" />
}
