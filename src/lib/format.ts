function parseEditionDate(value: string): Date {
  const match = value.match(/^(\d{4})-(\d{2})-(\d{2})$/);

  if (match) {
    const [, year, month, day] = match;
    return new Date(Date.UTC(Number(year), Number(month) - 1, Number(day)));
  }

  return new Date(value);
}

export function formatEditionDate(value: string): string {
  return new Intl.DateTimeFormat(undefined, {
    weekday: "long",
    month: "long",
    day: "numeric",
    timeZone: "UTC"
  }).format(parseEditionDate(value));
}

export function formatTime(value: string): string {
  return new Intl.DateTimeFormat(undefined, {
    hour: "numeric",
    minute: "2-digit"
  }).format(new Date(value));
}

export function joinLines(values: string[]): string {
  return values
    .map((value) => value.trim())
    .filter(Boolean)
    .join("\n");
}
