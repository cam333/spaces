import { NextRequest } from 'next/server';
import * as proxy from '@/lib/network/api/proxy';

type Ctx = { params: Promise<{ nodeId: string; path: string[] }> };

export async function GET(req: NextRequest, ctx: Ctx)  { return proxy.GET(req, ctx); }
export async function POST(req: NextRequest, ctx: Ctx) { return proxy.POST(req, ctx); }
