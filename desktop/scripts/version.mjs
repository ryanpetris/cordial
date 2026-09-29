export function version() {
  const value = process.env.CORDIAL_VERSION ?? "0.0.0";
  if (!/^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/.test(value) || value.includes("\n"))
    throw new Error("CORDIAL_VERSION must be MAJOR.MINOR.PATCH without a v prefix");
  return value;
}
