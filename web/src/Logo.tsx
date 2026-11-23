// Original Celoom mark (细胞绽放): central nucleus with four blooming petals and budding nodes.
// Keep this geometry in sync with the standalone SVG assets in public/brand.
export function LogoMark({ size = 32, className = '' }: { size?: number; className?: string }) {
  return <svg className={`logo-mark ${className}`} width={size} height={size} viewBox="0 0 32 32" fill="none" aria-hidden="true" focusable="false">
    <circle cx="16" cy="16" r="2.75" fill="currentColor" />
    <path d="M16 3.5C19 7.2 19 10.8 16 13C13 10.8 13 7.2 16 3.5Z" stroke="currentColor" strokeWidth="2.2" strokeLinejoin="round" />
    <path d="M16 28.5C19 24.8 19 21.2 16 19C13 21.2 13 24.8 16 28.5Z" stroke="currentColor" strokeWidth="2.2" strokeLinejoin="round" />
    <path d="M3.5 16C7.2 13 10.8 13 13 16C10.8 19 7.2 19 3.5 16Z" stroke="currentColor" strokeWidth="2.2" strokeLinejoin="round" />
    <path d="M28.5 16C24.8 13 21.2 13 19 16C21.2 19 24.8 19 28.5 16Z" stroke="currentColor" strokeWidth="2.2" strokeLinejoin="round" />
    <circle cx="7.5" cy="7.5" r="1.6" fill="currentColor" />
    <circle cx="24.5" cy="7.5" r="1.6" fill="currentColor" />
    <circle cx="7.5" cy="24.5" r="1.6" fill="currentColor" />
    <circle cx="24.5" cy="24.5" r="1.6" fill="currentColor" />
  </svg>;
}
