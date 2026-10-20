import { WORDMARK } from "./brand";

/** Header logo: the NOVIUM emblem, and the NOVIUM letters (coloured by the theme) over "Spatial Explorer". */
export function Brand() {
  return (
    <span className="brand" title="NOVIUM Spatial Explorer">
      <img className="brand-mark" src={`${import.meta.env.BASE_URL}novium-mark.svg`} alt="" />
      <span className="brand-text">
        <svg className="brand-word" viewBox={WORDMARK.viewBox} role="img" aria-label="NOVIUM">
          {WORDMARK.letters.map((l, i) => (
            <path key={i} d={l.d} fillRule={l.evenodd ? "evenodd" : undefined} />
          ))}
        </svg>
        <span className="brand-sub">Spatial Explorer</span>
      </span>
    </span>
  );
}
