const ENTITIES: Readonly<Record<string, string>> = {
  "&amp;": "&",
  "&lt;": "<",
  "&gt;": ">",
  "&quot;": '"',
  "&#39;": "'",
  "&apos;": "'",
  "&nbsp;": " ",
}

const SCRIPT_OR_STYLE = /<(script|style)\b[^>]*>[\s\S]*?<\/\1>/giu
const TAG = /<[^>]*>/gu
const ENTITY = /&(?:amp|lt|gt|quot|#39|apos|nbsp);/giu
const WHITESPACE = /\s+/gu

const decodeEntities = (text: string): string =>
  text.replace(ENTITY, (entity) => ENTITIES[entity.toLowerCase()] ?? entity)

/**
 * Search results are hostile input. Only inert text survives: no tags, no
 * scripts, no entities that a renderer would reinterpret, no autolink syntax.
 */
export const stripMarkup = (text: string): string =>
  decodeEntities(text.replace(SCRIPT_OR_STYLE, " ").replace(TAG, " "))
    .replace(/https?:\/\/\S+/giu, " ")
    .replace(/www\.\S+/giu, " ")
    .replace(WHITESPACE, " ")
    .trim()

export const capText = (text: string, maxChars: number): string => {
  const characters = [...text]
  return characters.length <= maxChars ? text : characters.slice(0, maxChars).join("")
}
