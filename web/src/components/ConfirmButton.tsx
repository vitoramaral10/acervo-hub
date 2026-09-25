import { type ReactNode, useState } from 'react'
import { Button, type ButtonProps } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'

/** Botão que pede confirmação num diálogo antes de uma ação que não volta. */
export function ConfirmButton({
  title,
  description,
  confirmLabel = 'Excluir',
  onConfirm,
  children,
  ...props
}: Omit<ButtonProps, 'onClick'> & {
  title: string
  description?: ReactNode
  confirmLabel?: string
  onConfirm: () => void
}) {
  const [open, setOpen] = useState(false)
  return (
    <>
      <Button {...props} onClick={() => setOpen(true)}>
        {children}
      </Button>
      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{title}</DialogTitle>
            {description && <DialogDescription>{description}</DialogDescription>}
          </DialogHeader>
          <DialogFooter>
            <Button variant="ghost" onClick={() => setOpen(false)}>
              Cancelar
            </Button>
            <Button
              variant="danger"
              onClick={() => {
                setOpen(false)
                onConfirm()
              }}
            >
              {confirmLabel}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  )
}
