# Lave Station

A Gtk Based GUI for Docker, implemented in Rust, using native control of Docker.

# Iterations

Version 1 - the essential application; window, persistent activity monitor indicator in home screen menu, tree menu on left hand side populated with a node containing Images available on the local device and another listing Containers (stopped and running) available on the local device. Selecting any item in the left hand side tree menu causes the metadata for that item to be rendered in the main part of the window on the right hand side. The root node of the tree, when selected (as it is by default at application start), causes the main part of the window to display various metadata for the local docker environment.

