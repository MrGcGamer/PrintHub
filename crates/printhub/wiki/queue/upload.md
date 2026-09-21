---
title: Uploading a job
summary: Model or G-code, and what each slicing setting does.
keywords: upload, stl, gcode, g-code, 3mf, slice, slicer, orcaslicer, build plate, plate, bed temperature, print profile, filament profile, infill, supports, orient, orientation, rotate, tilted, scale, resize, size, dimensions, drag and drop, layer height, file size, multicolour, multi-colour
order: 1
---
## Model or G-code

**An STL model** is sliced by PrintHub with OrcaSlicer and Elegoo's Centauri Carbon 2 profiles
for the [mounted nozzle](/wiki/printer/nozzle). It prints in one filament with the settings
below; use it when those are enough.

**G-code** you slice yourself. Use it for everything else: several colours or materials, custom
supports, modifiers, settings the form lacks. It has to say how many grams of filament it uses,
as slicers normally write, because the grams are what PrintHub matches against the spools.

A multi-colour print purges filament at every colour change, because the Centauri Carbon 2 feeds
every colour through one nozzle. Fewer colour changes waste less.

Other file types, such as 3MF projects, are not accepted. If the form offers no slicing settings,
this PrintHub cannot slice and takes G-code only.

The file can be dropped on the *File* box as well as chosen through it. Once a model is
chosen, the form gives its size in millimetres, measured in the browser from the file itself.

## Slicing settings

### Spool

The spool you mean to print with. Its material picks the filament profile, unless you choose
one, and its colour colours the sliced model. Confirming the job preselects it.

> Choosing a spool of a different material when confirming does not slice the model again: it
> still prints at the temperatures of the first spool's profile.

### Build plate

The plate the model will print on: *Textured PEI Plate* or *High Temp Plate*. The filament
profile sets a bed temperature for each, and the model is sliced for the one chosen here. The
[material pages](/wiki/filament/materials) list both temperatures. The job page shows the plate
as *Build plate*, for G-code too when the file names one.

> PrintHub cannot see which plate is on the printer, and the G-code holds the bed temperature
> for the plate it was sliced for. Swap the plate
> [before confirming the bed clear](/wiki/queue/waiting#nobody-has-confirmed-that-the-bed-is-clear).

### Print profile

Elegoo's print settings for the mounted nozzle. The name starts with the layer height: thinner
layers show less of the stepped surface and take longer. *Standard* is chosen by default.

### Filament profile

The temperatures, fan and flow for the material. *Match the spool's material* looks for the
Elegoo profile named exactly like the spool's material, so a spool of `PETG` prints with
*Elegoo PETG @ECC2*. When no profile has that name, the upload asks you to choose one.

Choose one yourself for anything Elegoo makes a separate profile for, such as a silk or matte
PLA or a *Rapid* filament, and use a *Generic* profile for materials Elegoo has none for. The
[material pages](/wiki/filament/materials) list each profile's settings.

### Infill

How much of the inside is filled, in percent. 0 leaves only the walls, top and bottom; 100 is
solid. More infill makes a part heavier and slower to print, and past a point stronger walls
help more than more infill, but wall counts can only be changed in G-code you slice yourself.

### Scale

Shrinks the model in all three directions by the same percentage, from 1 to 100; 100 prints it
at its own size. The size line under the file follows what is typed here, so the printed
measurements can be checked before uploading.

> A model cannot be enlarged here. OrcaSlicer's command line crashes on any factor above 1, and
> the fix for it has not reached a release yet. Enlarge in your own slicer and upload the
> G-code instead.

Shrinking saves less filament than it looks: walls and the top and bottom keep their thickness,
so a part at 50 % uses well over an eighth of the filament.

### Supports

Adds removable scaffolding under overhangs that would otherwise print in mid-air. Leave it off
for models designed to print without it: supports cost filament and time, and leave marks where
they touched.

### Auto-orient

Turns the model onto the side OrcaSlicer judges best to print on before slicing it. Use it for a
model saved at an odd angle: one resting on an edge or a point either needs supports or fails to
slice, because parts of it would print in mid-air.

> It overrides how the model was saved, so a tall part meant to print standing is laid on its
> side. Leave it off for a model already facing the way it should print.
