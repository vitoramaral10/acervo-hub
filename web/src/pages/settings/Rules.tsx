import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { type ReactNode, useEffect, useState } from 'react'
import { toast } from 'sonner'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Skeleton, Switch } from '@/components/ui/misc'
import { type DecisionRules, type RulesView, library } from '@/lib/api'

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

function Toggle({
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
          <Toggle
            id="legenda"
            label="Aceitar legenda embutida"
            checked={rules.aceitar_legenda_embutida}
            onChange={(on) => set('aceitar_legenda_embutida', on)}
          />
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
