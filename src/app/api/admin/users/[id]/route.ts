import * as usersById from '@/lib/admin/api/usersById';

type Ctx = { params: Promise<{ id: string }> };

export async function GET(req: Request, ctx: Ctx)    { return usersById.GET(req, ctx); }
export async function PUT(req: Request, ctx: Ctx)    { return usersById.PUT(req, ctx); }
export async function DELETE(req: Request, ctx: Ctx) { return usersById.DELETE(req, ctx); }
