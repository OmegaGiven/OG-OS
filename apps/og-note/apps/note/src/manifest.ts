import type { AppManifest } from '@og-suite/runtime'

export const notesManifest: AppManifest = {
  id: 'notes',
  name: 'Notes',
  route: '/notes',
  standaloneRoute: '/',
  capabilities: ['offline', 'remoteSave', 'collaboration'],
  toolbar: [
    { kind: 'dropdown', id: 'heading', label: 'Header', options: [
      { label: 'Paragraph', command: 'paragraph' },
      { label: 'Heading 1', command: 'heading-1' },
      { label: 'Heading 2', command: 'heading-2' },
    ] },
    { kind: 'button', id: 'bold', label: 'B', command: 'bold' },
    { kind: 'button', id: 'italic', label: 'I', command: 'italic' },
    { kind: 'button', id: 'save', label: 'Save', command: 'save' },
  ],
}

