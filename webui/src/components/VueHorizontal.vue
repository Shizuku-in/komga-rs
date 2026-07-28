<template>
  <div class="horizontal-list">
    <div
      v-if="canScrollBackward"
      class="horizontal-list__control horizontal-list__control--previous"
      @click="scroll(-1)"
    >
      <slot name="btn-prev"/>
    </div>
    <div ref="viewport" class="horizontal-list__viewport" @scroll="updateControls">
      <div class="horizontal-list__content">
        <slot/>
      </div>
    </div>
    <div
      v-if="canScrollForward"
      class="horizontal-list__control horizontal-list__control--next"
      @click="scroll(1)"
    >
      <slot name="btn-next"/>
    </div>
  </div>
</template>

<script lang="ts">
import {defineComponent} from 'vue'

export default defineComponent({
  name: 'VueHorizontal',
  data: () => ({
    canScrollBackward: false,
    canScrollForward: false,
    resizeObserver: undefined as ResizeObserver | undefined,
  }),
  mounted() {
    this.resizeObserver = new ResizeObserver(this.updateControls)
    this.resizeObserver.observe(this.$refs.viewport as HTMLElement)
    this.$nextTick(this.updateControls)
  },
  beforeUnmount() {
    this.resizeObserver?.disconnect()
  },
  methods: {
    updateControls() {
      const viewport = this.$refs.viewport as HTMLElement
      this.canScrollBackward = viewport.scrollLeft > 0
      this.canScrollForward = viewport.scrollLeft + viewport.clientWidth < viewport.scrollWidth - 1
    },
    scroll(direction: number) {
      const viewport = this.$refs.viewport as HTMLElement
      viewport.scrollBy({left: direction * viewport.clientWidth * 0.8, behavior: 'smooth'})
    },
  },
})
</script>

<style scoped>
.horizontal-list {
  position: relative;
}

.horizontal-list__viewport {
  overflow-x: auto;
  scrollbar-width: none;
}

.horizontal-list__viewport::-webkit-scrollbar {
  display: none;
}

.horizontal-list__content {
  display: inline-flex;
  min-width: 100%;
}

.horizontal-list__control {
  position: absolute;
  top: 50%;
  z-index: 1;
  transform: translateY(-50%);
}

.horizontal-list__control--previous {
  left: 0;
}

.horizontal-list__control--next {
  right: 0;
}
</style>
