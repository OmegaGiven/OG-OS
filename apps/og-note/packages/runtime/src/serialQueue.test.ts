import { describe, expect, it } from 'vitest'
import { createSerialQueue } from './index'

function delay(ms: number) {
  return new Promise<void>((resolve) => setTimeout(resolve, ms))
}

describe('createSerialQueue', () => {
  it('runs tasks in call order even when an earlier task is slower', async () => {
    const enqueue = createSerialQueue()
    const order: number[] = []

    const slow = enqueue(async () => {
      await delay(30)
      order.push(1)
    })
    const fast = enqueue(async () => {
      order.push(2)
    })

    await Promise.all([slow, fast])
    expect(order).toEqual([1, 2])
  })

  it('never overlaps two tasks — this is the exact save-race the app hit', async () => {
    // Regression coverage for the bug fixed in NotesApp.svelte: saveDocument()
    // read `lastSavedEditorText` at the top of its run and only wrote the
    // new value at the end. Two overlapping calls both read the same stale
    // baseline and corrupted the diff. A shared mutable "document" here
    // stands in for that baseline — if two enqueued tasks ever ran
    // concurrently, the second read would win over the first write and the
    // final value would silently lose an update.
    const enqueue = createSerialQueue()
    let document = ''
    let concurrentRuns = 0
    let maxConcurrentRuns = 0

    async function appendLikeSaveDocument(char: string) {
      return enqueue(async () => {
        concurrentRuns += 1
        maxConcurrentRuns = Math.max(maxConcurrentRuns, concurrentRuns)
        const previous = document // read baseline, same as saveDocument reading lastSavedEditorText
        await delay(5) // simulate cache/network I/O the real save awaits
        document = previous + char // write result, same as saveDocument committing lastSavedEditorText
        concurrentRuns -= 1
      })
    }

    await Promise.all([
      appendLikeSaveDocument('a'),
      appendLikeSaveDocument('b'),
      appendLikeSaveDocument('c'),
      appendLikeSaveDocument('d'),
    ])

    expect(maxConcurrentRuns).toBe(1)
    expect(document).toBe('abcd')
  })

  it('keeps running later tasks after an earlier task throws', async () => {
    const enqueue = createSerialQueue()
    const order: string[] = []

    await expect(
      enqueue(async () => {
        order.push('first')
        throw new Error('boom')
      }),
    ).rejects.toThrow('boom')

    await enqueue(async () => {
      order.push('second')
    })

    expect(order).toEqual(['first', 'second'])
  })

  it('propagates each task result independently to its own caller', async () => {
    const enqueue = createSerialQueue()
    const [a, b] = await Promise.all([enqueue(async () => 1), enqueue(async () => 2)])
    expect(a).toBe(1)
    expect(b).toBe(2)
  })
})
