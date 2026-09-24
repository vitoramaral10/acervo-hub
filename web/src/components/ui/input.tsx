import * as React from 'react'
import { cn } from '@/lib/utils'

export const Input = React.forwardRef<HTMLInputElement, React.InputHTMLAttributes<HTMLInputElement>>(
  ({ className, ...props }, ref) => (
    <input
      ref={ref}
      className={cn(
        'h-9 w-full min-w-0 rounded-md border border-border-strong bg-surface px-3 text-sm text-content shadow-sm transition-colors outline-none placeholder:text-content-subtle',
        'focus-visible:border-accent focus-visible:ring-3 focus-visible:ring-ring/25 focus-visible:outline-none',
        'aria-invalid:border-danger aria-invalid:ring-danger/20 disabled:cursor-not-allowed disabled:opacity-60',
        className,
      )}
      {...props}
    />
  ),
)
Input.displayName = 'Input'
