// Connection-request-by-id handler is not implemented. See ../route.ts for
// context; keep as 404 until the pairing flow lands.

export async function PUT() {
  return Response.json({ error: 'Not available' }, { status: 404 });
}
