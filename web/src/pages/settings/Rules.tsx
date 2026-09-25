import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Crown, Undo2 } from 'lucide-react'
import { type ReactNode, useEffect, useState } from 'react'
import { toast } from 'sonner'
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
import { type DecisionRules, type RulesView, library } from '@/lib/api'

const PROPERS = [
  { value: 'preferir_e_atualizar', label: 'Preferir e trocar pelo PROPER/REPACK' },
  { value: 'nao_atualizar', label: 'Preferir, mas não trocar o que já tem' },
  { value: 'nao_preferir', label: 'Ignorar PROPER/REPACK' },
] as const

/** Quem decide: o Radarr (regras vêm de lá) ou o acervo (regras daqui). */
export function OwnerCard({ view }: { view: RulesView }) {
  const queryClient = useQueryClient()
  const [confirming, setConfirming] = useState(false)
  const refresh = () => {
    for (const key of [['regras'], ['perfis'], ['formatos'], ['tamanhos'], ['biblioteca-opcoes']]) {
      void queryClient.invalidateQueries({ queryKey: key })
    }
  }
  const takeOver = useMutation({
    mutationFn: library.takeOver,
    onSuccess: () => {
      toast.success('O acervo-hub agora decide')
      setConfirming(false)
    },
    onError: (error: Error) => toast.error(error.message),
    onSettled: refresh,
  })
  const giveBack = useMutation({
    mutationFn: library.giveBack,
    onSuccess: () => toast.success('As regras voltaram a vir do Radarr'),
    onError: (error: Error) => toast.error(error.message),
    onSettled: refresh,
  })
  const acervo = view.dono === 'acervo'
  return (
    <section
      aria-labelledby="dono-titulo"
      className="flex flex-col gap-4 rounded-lg border border-border bg-surface p-6 sm:flex-row sm:items-center sm:justify-between"
    >
      <div>
        <div className="flex items-center gap-2">
          <h2 id="dono-titulo" className="text-lg font-semibold">
            {acervo ? 'O acervo-hub decide' : 'O Radarr decide'}
          </h2>
          <Badge tone={acervo ? 'success' : 'warning'}>{acervo ? 'acervo' : 'Radarr'}</Badge>
        </div>
        <p className="mt-1 max-w-[65ch] text-sm text-content-muted">
          {acervo
            ? 'Os filmes, perfis, formatos, tamanhos e regras são do acervo-hub e se editam aqui. Nada mais vem do Radarr.'
            : 'Filmes, perfis, formatos, tamanhos e regras vêm do Radarr e aqui só aparecem; o que você muda num filme vai para o Radarr. Assuma quando for desligar o Radarr.'}
        </p>
      </div>
      {acervo ? (
        view.tem_radarr && (
          <Button variant="ghost" loading={giveBack.isPending} onClick={() => giveBack.mutate()}>
            {!giveBack.isPending && <Undo2 aria-hidden="true" />}
            Devolver ao Radarr
          </Button>
        )
      ) : (
        <Button variant="primary" onClick={() => setConfirming(true)}>
          <Crown aria-hidden="true" />
          Assumir
        </Button>
      )}
      <Dialog open={confirming} onOpenChange={setConfirming}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>O acervo-hub assume?</DialogTitle>
            <DialogDescription>
              É o corte. O acervo-hub copia uma última vez filmes, perfis, formatos, tamanhos e regras do Radarr, adota
              todos os filmes (com os mesmos ids) e para de importar de lá.
            </DialogDescription>
          </DialogHeader>
          <ul className="list-disc space-y-1 pl-5 text-sm text-content-muted">
            <li>Filmes adicionados e editados pela tela ou por listas ficam só no acervo-hub.</li>
            <li>As listas de importação daqui começam a rodar.</li>
            <li>Aponte o Seerr para o acervo-hub: ele fala a mesma API do Radarr.</li>
            <li>
              A busca automática continua como está em Geral — ligue-a só quando o Radarr parar de baixar, senão os dois
              pegam o mesmo filme.
            </li>
          </ul>
          <DialogFooter>
            <Button variant="ghost" onClick={() => setConfirming(false)}>
              Cancelar
            </Button>
            <Button variant="primary" loading={takeOver.isPending} onClick={() => takeOver.mutate()}>
              Assumir
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </section>
  )
}

function Field({ id, label, help, children }: { id: string; label: string; help?: string; children: ReactNode }) {
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
  disabled: boolean
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
  const editable = view.dono === 'acervo'
  const [rules, setRules] = useState<DecisionRules>(view.regras)
  useEffect(() => setRules(view.regras), [view.regras])
  const dirty = JSON.stringify(rules) !== JSON.stringify(view.regras)
  const save = useMutation({
    mutationFn: () => library.saveRules(rules),
    onSuccess: () => toast.success('Regras salvas'),
    onError: (error: Error) => toast.error(error.message),
    onSettled: () => void queryClient.invalidateQueries({ queryKey: ['regras'] }),
  })
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
        <fieldset disabled={!editable} className="grid gap-4 sm:grid-cols-2">
          <legend className="mb-2 text-sm font-semibold">Tamanho e disco</legend>
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
          <Field id="folga" label="Folga mínima no disco (MB)" help="O que precisa sobrar depois do download.">
            <Input
              id="folga"
              type="number"
              min={0}
              value={rules.folga_minima_mb}
              onChange={(e) => set('folga_minima_mb', number(e.target.value))}
              aria-describedby="folga-ajuda"
            />
          </Field>
          <div className="sm:col-span-2">
            <Toggle
              id="pular-espaco"
              label="Não conferir o espaço livre"
              checked={rules.pular_checagem_de_espaco}
              disabled={!editable}
              onChange={(on) => set('pular_checagem_de_espaco', on)}
            />
          </div>
        </fieldset>

        <fieldset disabled={!editable} className="grid gap-4 sm:grid-cols-2">
          <legend className="mb-2 text-sm font-semibold">Releases</legend>
          <Field id="propers" label="PROPER e REPACK">
            <Select
              value={rules.propers}
              disabled={!editable}
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
            disabled={!editable}
            onChange={(on) => set('aceitar_legenda_embutida', on)}
          />
          <Toggle
            id="flags"
            label="Preferir flags do indexador"
            help="Freeleech, internal e afins pesam na escolha."
            checked={rules.preferir_flags_do_indexador}
            disabled={!editable}
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

        <fieldset disabled={!editable} className="grid gap-4 sm:grid-cols-2">
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
          <Field id="atraso-nota" label="Não esperar a partir da nota" help="Vazio: sempre espera.">
            <Input
              id="atraso-nota"
              type="number"
              value={rules.atraso.pular_acima_da_nota ?? ''}
              onChange={(e) =>
                set('atraso', {
                  ...rules.atraso,
                  pular_acima_da_nota: e.target.value.trim() === '' ? null : Math.trunc(Number(e.target.value)),
                })
              }
              aria-describedby="atraso-nota-ajuda"
            />
          </Field>
          <div className="sm:col-span-2">
            <Toggle
              id="atraso-melhor"
              label="Não esperar se já for a melhor qualidade do perfil"
              checked={rules.atraso.pular_se_melhor_qualidade}
              disabled={!editable}
              onChange={(on) => set('atraso', { ...rules.atraso, pular_se_melhor_qualidade: on })}
            />
          </div>
        </fieldset>

        {indexers.length > 0 && (
          <fieldset disabled={!editable}>
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

        {editable && (
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
        )}
      </form>
    </section>
  )
}

/** Carrega as regras uma vez para o cartão de dono e o formulário. */
export function useRules() {
  return useQuery({ queryKey: ['regras'], queryFn: library.rules })
}

export function RulesSkeleton() {
  return <Skeleton className="h-64 rounded-lg" />
}
