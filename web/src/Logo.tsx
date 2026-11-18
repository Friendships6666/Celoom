// Original JustAIAgent mark: an open J orbit and a rising inner route.
// Keep this geometry in sync with the standalone SVG assets in public/brand.
export function LogoMark({ size = 32, className = '' }: { size?: number; className?: string }) {
  return <svg className={`logo-mark ${className}`} width={size} height={size} viewBox="0 0 32 32" fill="none" aria-hidden="true" focusable="false">
    <path d="M15 5H26V18.5C26 24.3 22 28 16.5 28S7 24.3 7 19" stroke="currentColor" strokeWidth="3.5" strokeLinecap="round" strokeLinejoin="round" />
    <path d="M12 18L18.5 11.5V20" stroke="currentColor" strokeWidth="3.5" strokeLinecap="round" strokeLinejoin="round" />
    <circle cx="7" cy="8" r="2.25" fill="currentColor" />
  </svg>;
}
