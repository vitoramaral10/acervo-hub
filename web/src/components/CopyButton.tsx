import { Check, Copy } from 'lucide-react'
import { useEffect, useRef, useState } from 'react'
import { toast } from 'sonner'
import { Button } from '@/components/ui/button'

/**
 * Copia para a área de transferência. `navigator.clipboard` só existe em contexto seguro
 * (https ou localhost) e o hub também é aberto por http na rede local; nesse caso, ou se o
 * navegador recusar, cai num textarea fora da tela com `execCommand('copy')`.
 */
export async function copyText(text: string): Promise<boolean> {
  if (window.isSecureContext && navigator.clipboard) {
    try {
      await navigator.clipboard.writeText(text)
      return true
    } catch {
      // Permissão negada ou documento sem foco: tenta o caminho antigo.
    }
  }
  const area = document.createElement('textarea')
  area.value = text
  area.setAttribute('readonly', '')
  area.setAttribute('aria-hidden', 'true')
  area.style.cssText = 'position:fixed;top:0;left:-9999px;opacity:0'
  const focused = document.activeElement
  document.body.appendChild(area)
  try {
    area.select()
    return document.execCommand('copy')
  } catch {
    return false
  } finally {
    area.remove()
    if (focused instanceof HTMLElement) focused.focus()
  }
}

/** Botão que copia `text` e avisa por toast; `label` diz o que foi copiado (ex.: "o título"). */
export function CopyButton({ text, label, disabled }: { text: string; label: string; disabled?: boolean }) {
  const [done, setDone] = useState(false)
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined)
  useEffect(() => () => clearTimeout(timer.current), [])

  const copy = async () => {
    if (await copyText(text)) {
      toast.success(`Copiado: ${label}`)
      setDone(true)
      clearTimeout(timer.current)
      timer.current = setTimeout(() => setDone(false), 2000)
    } else {
      toast.error(`Não consegui copiar ${label}. Selecione o texto e copie à mão.`)
    }
  }

  return (
    <Button
      type="button"
      variant="secondary"
      size="sm"
      disabled={disabled || text === ''}
      onClick={() => void copy()}
      aria-label={`Copiar ${label}`}
    >
      {done ? <Check aria-hidden="true" /> : <Copy aria-hidden="true" />}
      {done ? 'Copiado' : 'Copiar'}
    </Button>
  )
}
