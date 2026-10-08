// Connection-request handler is not implemented. GET returns an empty pairing
// list so the hooks in src/hooks/use-network.ts render cleanly; POST stays 404.

export async function POST() {
  return Response.json({ error: 'Not available' }, { status: 404 });
}

export async function GET() {
  return Response.json({ incoming: [], outgoing: [] });
}
