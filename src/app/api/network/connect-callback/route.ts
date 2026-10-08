// Connection-callback handler is not implemented. The paired connection flow
// was scaffolded but never wired up; keep the route as a 404 until it lands.

export async function POST() {
  return Response.json({ error: 'Not available' }, { status: 404 });
}
